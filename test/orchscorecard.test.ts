// `scripts/orch-scorecard.cjs` — the per-PR orchestration scorecard (#2011 B1).
//
// WHAT THIS PINS, and why each pin can fail. The script's whole value is that two
// readers derive the SAME number from the same rows, so every counter is pinned to
// the audit/transcript SHAPE it reads, against a synthetic corpus in
// `test/fixtures/orchscorecard/` built so that no two counters share a value — a
// fixture whose axes are all the same constant cannot tell a working counter from a
// broken one (#1182).
//
// The corpus is two PRs plus a control:
//
//   #900  driven — `rd-*` rows, two lanes, three verdicts across two blocks, three
//         refusals under two reasons, one hold, one hand-back, a merge time.
//   #901  hand-routed — no `rd-*` row at all, a verdict, a human prompt, a system
//         notice, and NO merge time, so the window's fallback arm is exercised.
//   #902  named by nothing. This is the NEGATIVE CONTROL for every positive control
//         below: `rows_classified` is 0 here and non-zero for #900/#901, so an
//         assertion of "the mechanism ran" is fail-able rather than decorative.
//
// The transcript fixture is FIVE SYNTHETIC LINES written by hand. Real transcript
// content is private and never enters a fixture; the script reads only `timestamp`
// and `message.usage` and this file pins that it reads nothing else usefully — the
// `user` line carries no usage and must be skipped, and the duplicate `assistant`
// line carrying one message id twice must collapse to a single turn.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { execFileSync } from 'node:child_process';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const require_ = createRequire(import.meta.url);
const here = path.dirname(fileURLToPath(import.meta.url));
const root = path.resolve(here, '..');
const scriptPath = path.join(root, 'scripts', 'orch-scorecard.cjs');
const fixtures = path.join(here, 'fixtures', 'orchscorecard');

const sc = require_(scriptPath) as any;

const AUDIT = path.join(fixtures, 'audit.jsonl');
const USAGE = path.join(fixtures, 'usage.json');
const AGENTS = path.join(fixtures, 'agents.json');
const TRANSCRIPT = path.join(fixtures, 'transcript.jsonl');
const PR_META = path.join(fixtures, 'pr-meta.json');

// One run of the whole pipeline through the CLI, reused by the per-counter tests.
// Going through `main` rather than the internals is deliberate: the argument parsing,
// the file reads and the JSON shape are as much of the contract as the counters.
function runScorecard(extraArgs: string[] = []): any {
  const out = execFileSync(process.execPath, [
    scriptPath,
    '--audit', AUDIT,
    '--usage', USAGE,
    '--agents', AGENTS,
    '--transcript', TRANSCRIPT,
    '--pr-meta', PR_META,
    '--pr', '900', '--pr', '901', '--pr', '902',
    ...extraArgs,
  ], { encoding: 'utf8', maxBuffer: 32 * 1024 * 1024 });
  return out;
}

const REPORT = JSON.parse(runScorecard());
const card = (pr: number) => REPORT.prs.find((c: any) => c.pr === pr);

// ---------------------------------------------------------------------------
// Positive control: the mechanism ran at all.
// ---------------------------------------------------------------------------

test('positive control: rows were classified, and the control PR classified none', () => {
  assert.ok(REPORT.coverage.rows_classified > 0, 'no audit row was classified — the scorecard did not run');
  assert.equal(REPORT.coverage.rows_classified, 22);
  assert.ok(card(900).rows_classified > 0);
  assert.ok(card(901).rows_classified > 0);
  // The negative control. Without this the assertion above passes for a scorecard
  // that classifies every row it sees regardless of which PR it names.
  assert.equal(card(902).rows_classified, 0);
  assert.equal(card(902).orchestrator.wakes_total, 0);
  assert.equal(card(902).review.rounds, 0);
  assert.equal(card(902).delegates.count, 0);
  assert.equal(card(902).windows.pr, null);
  assert.equal(card(902).windows.loop, null);
  assert.equal(card(902).share.orchestrator_pct_raw, null);
});

// ---------------------------------------------------------------------------
// Wake classification — the shape, one class at a time.
// ---------------------------------------------------------------------------

test('classifyWake: each class is decided by its leading shape', () => {
  assert.equal(sc.classifyWake('[orrerix] w-13 reports progress (#900): pushed'), 'delegate-progress');
  assert.equal(sc.classifyWake('[orrerix] w-13 reports done (#900): green'), 'delegate-done');
  assert.equal(sc.classifyWake('[orrerix] rev-12 reports approved (#900): pass'), 'reviewer-report');
  assert.equal(sc.classifyWake('[orrerix] rev-12 reports request_changes (#900): three findings'), 'reviewer-report');
  assert.equal(sc.classifyWake('[orrerix] w-13 reports blocked (#900): needs a call'), 'delegate-blocked');
  assert.equal(sc.classifyWake('[orrerix] message from w-13 (#900): answering inline'), 'delegate-blocked');
  assert.equal(sc.classifyWake('[orrerix] rev-12 (rev-final) recorded verdict PASS on PR #900: gate satisfied'), 'verdict-notice');
  assert.equal(sc.classifyWake('[orrerix] PR #900 checks: SUCCESS — all 5 checks passed.'), 'system-notice');
  assert.equal(sc.classifyWake('[orrerix] run 33346168758: completed — conclusion: success.'), 'system-notice');
  // NEGATIVE CONTROL: a human typing into the pane is `other`, never a report class,
  // and an empty/absent text does not throw or silently become a report.
  assert.equal(sc.classifyWake('please look at #901 by hand'), 'other');
  assert.equal(sc.classifyWake(''), 'other');
  assert.equal(sc.classifyWake(undefined), 'other');
  // The tie-break: a reviewer's `reports approved` must NOT fall into the generic
  // delegate classes, and a verdict echo must not fall into `system-notice`.
  assert.notEqual(sc.classifyWake('[orrerix] rev-12 reports approved (#900): pass'), 'delegate-progress');
  assert.notEqual(sc.classifyWake('[orrerix] rev-12 (rev-final) recorded verdict FAIL on PR #900: x'), 'system-notice');
  // Every class the script declares is reachable by some shape above.
  assert.deepEqual(sc.WAKE_KINDS, [
    'delegate-progress', 'delegate-done', 'reviewer-report',
    'delegate-blocked', 'verdict-notice', 'system-notice', 'other',
  ]);
});

test('prTokenRe: the trailing-digit guard stops a prefix match', () => {
  assert.ok(sc.prTokenRe(1751).test('bumped #1751 to green'));
  assert.ok(sc.prTokenRe(1751).test('see #1751.'));
  // Without the guard `#175` matches inside `#1751` and every counter for the
  // short-numbered PR inherits the long-numbered one's rows.
  assert.equal(sc.prTokenRe(175).test('bumped #1751 to green'), false);
  assert.equal(sc.prTokenRe(900).test('#9001'), false);
  assert.ok(sc.prTokenRe(900).test('#900'));
});

test('rowNamesPr: `detail.pr` is structural, the token is the fallback', () => {
  assert.ok(sc.rowNamesPr({ detail: { pr: 900 } }, 900));
  assert.ok(sc.rowNamesPr({ detail: { text: 'work on #900' } }, 900));
  assert.equal(sc.rowNamesPr({ detail: { pr: 901 } }, 900), false);
  assert.equal(sc.rowNamesPr({ detail: {} }, 900), false);
  assert.equal(sc.rowNamesPr({}, 900), false);
});

// ---------------------------------------------------------------------------
// Windows.
// ---------------------------------------------------------------------------

test('windows: the loop window is the rd rows, the PR window ends at the merge', () => {
  const c = card(900);
  assert.deepEqual(c.windows.loop, {
    start_ms: 1700000120000, end_ms: 1700000780000, tail_ms: 600000, span_h: 0.18,
  });
  assert.deepEqual(c.windows.pr, {
    start_ms: 1700000000000,   // the `agent-spawn` row whose brief names #900
    end_ms: 1700001200000,     // `merged_at` from --pr-meta
    end_source: 'merged_at',
    tail_ms: 600000,
    span_h: 0.33,
  });
});

test('windows: with no merge time and no rd row the end falls back, and says so', () => {
  const c = card(901);
  assert.equal(c.windows.loop, null, 'a hand-routed PR has no loop window');
  assert.equal(c.windows.pr.end_source, 'last-naming-row');
  assert.equal(c.windows.pr.start_ms, 1700000030000);
  assert.equal(c.windows.pr.end_ms, 1700000390000);
  assert.equal(c.windows.pr.span_h, 0.1);
});

test('inWindow: the +10 min tail is inclusive and one-sided', () => {
  const win = { start_ms: 1000, end_ms: 2000, tail_ms: 600000 };
  assert.equal(sc.inWindow(999, win), false);
  assert.ok(sc.inWindow(1000, win));
  assert.ok(sc.inWindow(2000 + 600000, win));
  assert.equal(sc.inWindow(2000 + 600001, win), false);
  assert.equal(sc.inWindow(1500, null), false);
});

// ---------------------------------------------------------------------------
// Orchestrator attention.
// ---------------------------------------------------------------------------

test('wakes: counted only for prompts delivered to an orchestrator pane, inside the window, naming the PR', () => {
  const a = card(900).orchestrator;
  assert.equal(a.wakes_total, 3);
  assert.deepEqual(a.wakes_by_kind, {
    'delegate-progress': 1, 'delegate-done': 1, 'reviewer-report': 0,
    'delegate-blocked': 0, 'verdict-notice': 1, 'system-notice': 0, other: 0,
  });
  const b = card(901).orchestrator;
  assert.equal(b.wakes_total, 5);
  assert.deepEqual(b.wakes_by_kind, {
    'delegate-progress': 0, 'delegate-done': 0, 'reviewer-report': 1,
    'delegate-blocked': 2, 'verdict-notice': 0, 'system-notice': 1, other: 1,
  });
  // The corpus holds one `[orrerix]` prompt naming #900 that went to a WORKER pane
  // (the driver's own hand-back). It is not an orchestrator wake and must not be one
  // here — deleting the `orchIds.has(detail.to)` test makes #900 read 4.
  assert.equal(a.wakes_total + b.wakes_total, 8);
  assert.equal(REPORT.group.files[0].orchestrator_wakes, 8);
});

test('loop notices: the corrected count and the #1778 S5 count differ by the delegate-pane deliveries', () => {
  const a = card(900).orchestrator;
  // Inside the loop window (1700000120000 .. 1700000780000 + 10 min) two
  // `[orrerix]` prompts reached the orchestrator; a third reached w-13.
  assert.equal(a.loop_notices, 2);
  assert.equal(a.loop_notices_any_pane_s5, 3);
  // A hand-routed PR has no loop window, so both are zero however many notices the
  // orchestrator got.
  assert.equal(card(901).orchestrator.loop_notices, 0);
  assert.equal(card(901).orchestrator.loop_notices_any_pane_s5, 0);
});

// ---------------------------------------------------------------------------
// Review rounds and the driver.
// ---------------------------------------------------------------------------

test('review rounds: verdicts keyed by block and verdict, from `review-verdict` rows', () => {
  // Four verdicts, one of them recorded AFTER the merge (see the outside-window
  // counter below) — a round is a round wherever the row lands.
  assert.equal(card(900).review.rounds, 4);
  assert.deepEqual(card(900).review.by_block, {
    'rev-std': { fail: 1, pass: 1 },
    'rev-final': { pass: 2 },
  });
  // A hand-routed PR still has rounds — this counter predates the driver entirely,
  // which is what makes a before/after comparison possible.
  assert.equal(card(901).review.rounds, 1);
  assert.deepEqual(card(901).review.by_block, { 'rev-final': { escalate: 1 } });
});

test('driver counters: each `rd-*` action lands in its own bucket, reasons kept apart', () => {
  const d = card(900).driver;
  assert.equal(d.drives, 1);
  assert.equal(d.lane_spawns, 2);
  assert.equal(d.hand_backs, 1);
  assert.equal(d.refused, 3);
  assert.equal(d.held, 1);
  assert.equal(d.satisfied, 1);
  assert.deepEqual(d.refused_by_reason, { 'lane-spawn-refused': 2, 'worker-unresumable': 1 });
  assert.deepEqual(d.held_by_reason, { 'drive-stalled': 1 });
  // Untouched buckets stay zero rather than absent, so a table cell is never blank
  // for a counter that simply did not fire.
  assert.equal(d.cancelled, 0);
  assert.equal(d.consumed, 0);
  assert.equal(d.ci_green, 0);
  // NEGATIVE CONTROL: no `rd-*` row names #901, so every driver counter is zero and
  // the reason maps are empty — not inherited from #900.
  const h = card(901).driver;
  assert.equal(h.drives + h.lane_spawns + h.hand_backs + h.refused + h.held + h.satisfied, 0);
  assert.deepEqual(h.refused_by_reason, {});
  assert.deepEqual(h.held_by_reason, {});
});

// ---------------------------------------------------------------------------
// Tokens.
// ---------------------------------------------------------------------------

test('dedupeTranscriptTurns: one API response is one turn, keeping the largest output', () => {
  const entries = [
    { type: 'assistant', timestamp: '2023-11-14T22:15:00.000Z', message: { id: 'a', usage: { input_tokens: 1, output_tokens: 2, cache_read_input_tokens: 3, cache_creation_input_tokens: 4 } } },
    { type: 'assistant', timestamp: '2023-11-14T22:15:00.500Z', message: { id: 'a', usage: { input_tokens: 1, output_tokens: 40, cache_read_input_tokens: 3, cache_creation_input_tokens: 4 } } },
    { type: 'user', timestamp: '2023-11-14T22:15:01.000Z', message: { role: 'user' } },
    { type: 'assistant', timestamp: '2023-11-14T22:15:02.000Z', message: { id: 'b' } },
    { type: 'assistant', message: { id: 'c', usage: { input_tokens: 1, output_tokens: 1 } } },
  ];
  const out = sc.dedupeTranscriptTurns(entries);
  assert.equal(out.turns.length, 1, 'the duplicated message id must collapse to one turn');
  assert.equal(out.turns[0].usage.output, 40, 'the mid-stream line under-reports output and must lose');
  // Three lines carry no usable usage: the user line, the assistant line with no
  // usage, and the assistant line with no timestamp (it cannot be windowed).
  assert.equal(out.non_usage_lines, 3);
  assert.equal(out.without_id, 0);
});

test('orchestrator tokens: the sum of deduped transcript turns stamped inside the PR window', () => {
  const a = card(900).orchestrator.tokens_window;
  // Turns at +100 s and +1100 s are inside #900's window; the one at +5000 s is not.
  assert.deepEqual(a, { input: 17, cache_read: 3000, cache_creation: 23, output: 61, total: 3101, turns: 2 });
  // #901's window ends 710 s earlier, so it sees only the first turn — the same
  // transcript, a different window, a different number. A scorecard that ignored the
  // window would report 3101 here too.
  const b = card(901).orchestrator.tokens_window;
  assert.deepEqual(b, { input: 10, cache_read: 1000, cache_creation: 20, output: 50, total: 1080, turns: 1 });
  // NEGATIVE CONTROL: no window, no tokens.
  assert.equal(card(902).orchestrator.tokens_window.total, 0);
  assert.equal(card(902).orchestrator.tokens_window.turns, 0);
});

test('orchestrator tokens are also apportioned by this PR\'s share of the window\'s wakes', () => {
  const a = card(900).orchestrator;
  assert.deepEqual(a.wake_share, { pr_wakes: 3, window_wakes: 8, share: 0.38 });
  assert.equal(a.tokens_attributed.total, 1163); // round(3101 * 3/8) = round(1162.875)
  const b = card(901).orchestrator;
  assert.deepEqual(b.wake_share, { pr_wakes: 5, window_wakes: 8, share: 0.63 });
  assert.equal(b.tokens_attributed.total, 675); // 1080 * 5/8, exact
});

test('delegate tokens: attributed agents only, weighted, orchestrator excluded', () => {
  const d = card(900).delegates;
  assert.equal(d.count, 3);
  assert.deepEqual(d.agents.map((a: any) => a.agent).sort(), ['rev-11', 'rev-12', 'w-13']);
  // rev-12 reviewed both PRs, so half its lifetime tokens land on each (H4).
  assert.deepEqual(d.tokens, { input: 500, cache_read: 4500, cache_creation: 250, output: 100, total: 5350, turns: 3 });
  const e = card(901).delegates;
  assert.equal(e.count, 2);
  assert.deepEqual(e.agents.map((a: any) => a.agent).sort(), ['rev-12', 'w-14']);
  assert.equal(e.tokens.total, 6070);
  // The orchestrator's own `usage.json` row must never be counted as a delegate —
  // it would be double-counted against the transcript figure. orch-10 IS attributed
  // to #900 by its restore brief (see the coverage test), so this is the role filter
  // being exercised, not an agent that was never a candidate.
  for (const c of REPORT.prs) {
    assert.equal(c.delegates.agents.some((a: any) => a.agent === 'orch-10'), false);
  }
});

test('orchestrator share is reported both raw and apportioned', () => {
  assert.equal(card(900).share.orchestrator_pct_raw, 36.69);   // 3101 / 8451
  assert.equal(card(900).share.orchestrator_pct_attributed, 17.86); // 1163 / 6513
  assert.equal(card(901).share.orchestrator_pct_raw, 15.1);    // 1080 / 7150
  assert.equal(card(901).share.orchestrator_pct_attributed, 10.01); // 675 / 6745
});

// ---------------------------------------------------------------------------
// Attribution and coverage.
// ---------------------------------------------------------------------------

test('attribution: a structural row beats a text join, and the tier is reported', () => {
  const byAgent = new Map<string, any>();
  for (const c of REPORT.prs) for (const a of c.delegates.agents) byAgent.set(a.agent + '@' + c.pr, a);
  // w-13 is named by an `rd-handback` row carrying both the agent and the PR.
  assert.equal(byAgent.get('w-13@900').tier, 'structural');
  // rev-11 / rev-12 come from `review-verdict` (`actor` + `detail.pr`).
  assert.equal(byAgent.get('rev-11@900').tier, 'structural');
  assert.equal(byAgent.get('rev-12@901').tier, 'structural');
  // w-14 has no such row anywhere — only its spawn brief names #901.
  assert.equal(byAgent.get('w-14@901').tier, 'text');
  assert.equal(byAgent.get('w-14@901').weight, 1);
  assert.equal(byAgent.get('rev-12@900').weight, 0.5);
  assert.equal(byAgent.get('rev-12@900').shared_with_prs, 2);
});

test('attribution: a text join outside the PR window is ignored', () => {
  const rows = [
    { action: 'agent-spawn', ts_ms: 500, detail: { agent: 'w-in', task: 'fix #900' } },
    { action: 'agent-spawn', ts_ms: 99999, detail: { agent: 'w-out', task: 'still citing #900 days later' } },
  ];
  const windows = new Map([[900, { pr: { start_ms: 0, end_ms: 1000, tail_ms: 0 }, loop: null }]]);
  const out = sc.attributeAgents(rows, [900], windows);
  assert.deepEqual([...out.keys()], ['w-in']);
  assert.equal(out.get('w-in').tier, 'text');
  // POSITIVE CONTROL for the line above: widen the window and `w-out` IS picked up,
  // so the empty result is the window bound working rather than the join never firing.
  const wide = new Map([[900, { pr: { start_ms: 0, end_ms: 1000000, tail_ms: 0 }, loop: null }]]);
  assert.deepEqual([...sc.attributeAgents(rows, [900], wide).keys()].sort(), ['w-in', 'w-out']);
});

test('coverage: unattributed and split agents are named, not swallowed', () => {
  const cov = REPORT.coverage;
  // w-15 was spawned inside #900's window and its brief names no PR: it is exactly
  // the gap #2011 B2 exists to close, so it must appear by name.
  assert.deepEqual(cov.agents_unattributed_spawned_in_window, ['w-15']);
  assert.deepEqual(cov.agents_split_across_prs, [{ agent: 'rev-12', prs: [900, 901], tier: 'structural' }]);
  // POSITIVE CONTROL for the delegate test's "orchestrator excluded" assertion: the
  // corpus DOES attribute orch-10 to #900 (its restore brief names the PR), so that
  // assertion is about the role filter rather than about orch-10 never being seen.
  assert.equal(cov.agents_attributed, 5);
  assert.equal(cov.agents_unattributed_spawned_in_window.includes('orch-10'), false);
  assert.equal(cov.audit_rows_read, 27);
  assert.equal(cov.audit_parse_errors, 0);
  assert.equal(cov.usage_rows_without_agent_id, 0);
  assert.deepEqual(cov.transcripts, [{
    path: TRANSCRIPT, lines: 5, assistant_usage_lines: 4, deduped_turns: 3, usage_rows_without_id: 0,
  }]);
});

test('coverage: rows inside a window that no counter consumed are reported by action', () => {
  // Not every row naming a PR is a counter's input. Those that are not are named, so
  // "the scorecard saw everything" is checkable rather than assumed.
  assert.deepEqual(card(900).rows_unclassified_in_window, { 'agent-spawn': 2, prompt: 1 });
  assert.deepEqual(card(901).rows_unclassified_in_window, { 'agent-spawn': 1 });
  // A `review-verdict` / `rd-*` row is matched structurally on `detail.pr`, so it counts
  // wherever it occurs — wider than the PR window. How many did is reported rather than
  // left for a reader to wonder about. The corpus carries one such row on #900 (a verdict
  // recorded after the merge) and none on #901.
  assert.equal(card(900).rows_counted_outside_pr_window, 1); // the post-merge verdict
  // NEGATIVE CONTROL: #901 has no such row, so a non-zero here is not a constant.
  assert.equal(card(901).rows_counted_outside_pr_window, 0);
});

test('coverage: every heuristic is declared with an id, a statement and its structural fix', () => {
  assert.ok(REPORT.coverage.heuristics.length >= 7);
  const ids = REPORT.coverage.heuristics.map((h: any) => h.id);
  assert.deepEqual(ids, [...new Set(ids)], 'heuristic ids must be unique');
  for (const h of REPORT.coverage.heuristics) {
    assert.match(h.id, /^H\d+$/);
    assert.ok(h.what.length > 20, `heuristic ${h.id} has no statement`);
    assert.ok(h.fix.length > 10, `heuristic ${h.id} names no structural fix`);
  }
});

// ---------------------------------------------------------------------------
// Group totals and rendering.
// ---------------------------------------------------------------------------

test('group totals: per-file wake census, independent of any PR selection', () => {
  assert.equal(REPORT.group.files.length, 1);
  const f = REPORT.group.files[0];
  assert.equal(f.rows, 27);
  assert.equal(f.orchestrator_wakes, 8);
  assert.equal(f.prompt_typed_to_orchestrator, 0);
  assert.deepEqual(f.wakes_by_kind, {
    'delegate-progress': 1, 'delegate-done': 1, 'reviewer-report': 1,
    'delegate-blocked': 2, 'verdict-notice': 1, 'system-notice': 1, other: 1,
  });
  // The per-file totals must sum to the per-file wake count, or a class is missing.
  const summed = Object.values(f.wakes_by_kind).reduce((a: number, b: any) => a + b, 0);
  assert.equal(summed, f.orchestrator_wakes);
});

test('--cut reproduces a historical measurement on a log that has since grown', () => {
  const cut = JSON.parse(runScorecard(['--cut', '1700000500000']));
  const f = cut.group.files[0];
  assert.equal(f.rows, 20, 'rows after the cut instant are dropped');
  assert.ok(f.rows < REPORT.group.files[0].rows);
  // The cut lands between the first verdict and the hand-back, so #900 keeps one
  // round and loses the rest — a non-zero survivor, so this is not the vacuous
  // 'everything is gone' reading of a cut.
  assert.equal(cut.prs.find((c: any) => c.pr === 900).review.rounds, 1);
  assert.equal(cut.prs.find((c: any) => c.pr === 900).driver.lane_spawns, 2);
  assert.equal(cut.prs.find((c: any) => c.pr === 900).driver.hand_backs, 0);
  assert.equal(cut.prs.find((c: any) => c.pr === 900).driver.satisfied, 0);
  // ... and the transcript is cut too: only the +100 s turn survives.
  assert.equal(cut.prs.find((c: any) => c.pr === 900).orchestrator.tokens_window.turns, 1);
});

test('the GFM table renders one row per PR with the counters the benchmark quotes', () => {
  const table = sc.renderPrTable(REPORT.prs);
  const lines = table.split('\n');
  assert.equal(lines.length, 2 + REPORT.prs.length);
  assert.ok(lines[0].startsWith('| PR |') && lines[0].endsWith('|'));
  assert.match(lines[1], /^\|(-{3}\|)+$/);
  // Every row has the same cell count as the header — a mismatch renders as a broken
  // table on github.com rather than failing anywhere.
  const cells = (l: string) => l.split('|').length;
  for (const l of lines) assert.equal(cells(l), cells(lines[0]));
  const row900 = lines.find((l) => l.startsWith('| #900 '))!;
  assert.match(row900, /\| 2 \| 1 \| 3 \| 1 \|/); // lane spawns, hand-backs, refused, held
  assert.match(row900, /\| 2 \(3\) \|/);          // corrected notices (S5's own count)
  const row902 = lines.find((l) => l.startsWith('| #902 '))!;
  assert.match(row902, /\| — \| — \|/);           // no loop window
});

test('the CLI prints usage instead of throwing when given nothing', () => {
  const out = execFileSync(process.execPath, [scriptPath], { encoding: 'utf8' });
  assert.match(out, /orch-scorecard/);
  assert.match(out, /--audit/);
  assert.match(out, /--transcript/);
});

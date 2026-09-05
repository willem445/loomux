#!/usr/bin/env node
'use strict';
// mutation-ledger — build the red-before-green / mutation ledger from the runs' own
// logs, and re-check a posted one against them (#2507).
//
// WHY THIS EXISTS. A wave of scratch rounds produces a table in the PR body: per round,
// the run id, the head SHA that run measured, which tests reddened, and the suite's
// passed/failed split. Today a worker transcribes all of that by hand, and every fix push
// stales one or more figures — receipt churn was the second-largest orchestrator cost of
// the beta8 round (#2321), with #2239 and #2308 each spending three-plus review rounds on
// a body whose CODE never moved. Every one of those figures is already printed in a log
// GitHub is keeping. This reads them out of it.
//
// THE ONE RULE THE SCRIPT ENFORCES ON ITSELF: every figure is READ, never derived. The
// suite total is `passed + failed` off ONE `test result:` line, not a sum across targets
// and not last round's number adjusted by arithmetic. A run whose log yields no totals for
// the requested target is an ERROR — never an empty ledger, because an empty ledger and a
// clean one render identically and the empty one is the lie (CLAUDE.md, "a sweep whose
// success shape is ZERO needs a positive control").
//
// TWO MODES.
//
//   --rows <file>   GENERATE. Reads a rows file naming the base (banked green) run and one
//                   row per scratch round, and prints the markdown table plus the
//                   `Round N — k tests (P/F)` bullets, each row dated to the head SHA its
//                   own run reports.
//   --check <pr>    RE-CHECK. Reads a POSTED body's ledger table and bullets and re-reads
//                   every figure out of the runs they cite, printing OK/MISMATCH/CHECK per
//                   figure. This is the reviewer's instrument.
//
// WHERE THE SEAM WITH `pr-body-check.cjs` IS. That script re-measures a body against the
// PR's own HEAD — blob sizes, diffstat, SHA resolution, whether a cited run exists and what
// `headSha` it reports. It never opens a log, so it cannot tell you whether "588 passed; 6
// failed" is what the run SAID; it can only tell you the run is real. This one opens the
// log and cannot see head at all. Run both: `pr-body-check` for the body's relationship to
// the tree, `mutation-ledger --check` for the ledger's relationship to the runs.
//
// THREE SEVERITIES, matching `pr-body-check`'s contract so a worker reads one vocabulary:
//   MISMATCH — a figure DISAGREES with the log. Must be zero before `report(done)`.
//   CHECK    — narrowed to a judgment the script cannot make (which line of a panic block a
//              human chose to quote; a run whose log could not be fetched).
//   OK       — re-derived and equal.
// Exits 0 always. It is a report, never a gate.
//
// PURE CORE, INJECTED FACTS. Everything below `parseRunLog` is pure: `buildLedger(spec,
// logs)` and `checkBody(body, logs)` take log TEXT, never a run id. `fetchLog` is the only
// thing that shells out. The suite (`test/mutationledger.test.ts`) drives the pure half
// over scrubbed fixture logs, so it is offline and never runs `gh`.
//
// Dependency-free CJS: the root package.json is `"type": "module"`, so a `.js` file here
// would be ESM and `require` a ReferenceError.

const fs = require('node:fs');
const path = require('node:path');
const { execFileSync } = require('node:child_process');

// How wide a quoted failure line is cut. 118 is what #2239's ledger used, and the column
// is unreadable much wider; it is an option because it is a presentation choice, not a
// measurement.
const ABRIDGE = 118;

const NUMBER_WORDS = {
  one: 1, two: 2, three: 3, four: 4, five: 5, six: 6,
  seven: 7, eight: 8, nine: 9, ten: 10, eleven: 11, twelve: 12,
};

// ---------------------------------------------------------------------------
// Log shaping
// ---------------------------------------------------------------------------

// CSI sequences plus the odd OSC. Cargo colours `Running` and `error`, so a `Running` line
// is invisible to a pattern anchored on the bare word until this has run — the first draft
// of this parser found zero targets in a real log for exactly that reason.
//
// TWO SPELLINGS OF ESC, and both are load-bearing. GitHub’s stored log carries the real
// byte (0x1B). A log fetched through the sandboxed shell agents run under here arrives with
// every 0x1B rewritten to the two-character CARET NOTATION `^[` — measured on run
// 33928049423’s log as fetched into this worktree, which contains ZERO 0x1B bytes. A
// stripper that knows only the byte leaves `^[[1m^[[92m     Running^[[0m …` intact,
// recognises no target at all, and the figures come back EMPTY rather than wrong — which
// is exactly the shape `selectSuite` below turns into an error rather than a blank row. The
// caret arm is anchored to the whole CSI shape (`^[`, `[`, parameters, a final byte), so a
// bare `^[` in test output is not eaten; a Rust debug string spells the escape `\u{1b}`
// and is untouched either way.
const ESC = '(?:\\u001b|\\^\\[)';
const ANSI_OSC = new RegExp(`${ESC}\\][^\\u0007\\u001b]*(?:\\u0007|${ESC}\\\\)`, 'g');
const ANSI_CSI = new RegExp(`${ESC}\\[[0-9;?]*[ -/]*[@-~]`, 'g');

function stripAnsi(s) {
  return String(s).replace(ANSI_OSC, '').replace(ANSI_CSI, '').replace(/\r/g, '');
}

const TS_RE = /^\uFEFF?(\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d+Z) ?/;

// `gh run view <id> --log` prints `<job>\t<step>\t<timestamp> <content>`; a per-job log
// (`gh api .../jobs/<id>/logs`) prints `<timestamp> <content>` alone. Both are accepted,
// because a fixture cut from either is the same evidence. The split is index-based rather
// than `split('\t')`: test output legitimately contains tabs, and splitting on all of them
// silently truncates a failure line at its first one.
function parseRunLog(text) {
  const out = [];
  const lines = String(text).replace(/\r\n/g, '\n').split('\n');
  for (const raw of lines) {
    if (raw === '') continue;
    let job = '';
    let step = '';
    let rest = raw;
    const t1 = raw.indexOf('\t');
    if (t1 >= 0) {
      const t2 = raw.indexOf('\t', t1 + 1);
      if (t2 >= 0 && TS_RE.test(raw.slice(t2 + 1))) {
        job = raw.slice(0, t1);
        step = raw.slice(t1 + 1, t2);
        rest = raw.slice(t2 + 1);
      }
    }
    const m = rest.match(TS_RE);
    const ts = m ? m[1] : null;
    if (m) rest = rest.slice(m[0].length);
    out.push({ job, step, ts, text: stripAnsi(rest) });
  }
  return out;
}

// `target/debug/deps/loomux_engine-a2d609552867cb4e` -> `loomux_engine`. The hash suffix
// changes on every build, so it can never be part of a stable target name.
function targetStem(artifact) {
  const base = String(artifact).split(/[\\/]/).pop() || '';
  const cut = base.replace(/\.exe$/i, '').replace(/-[0-9a-f]{8,}$/i, '');
  return cut || base;
}

const RUNNING_RE = /^\s*Running\s+(.*?)\s+\((.+)\)\s*$/;
const RESULT_RE = /^test result:\s+(ok|FAILED)\.\s+(\d+)\s+passed;\s+(\d+)\s+failed;\s+(\d+)\s+ignored;\s+(\d+)\s+measured;\s+(\d+)\s+filtered out/;
const STDOUT_RE = /^----\s+(\S+)\s+stdout\s+----\s*$/;
const PANICKED_RE = /^thread '.*' (?:\([0-9]+\) )?panicked at .+:$/;
const RUNNING_COUNT_RE = /^running (\d+) tests?$/;
const DOCTESTS_RE = /^\s*Doc-tests\s+(\S+)\s*$/;

// Read every cargo test target out of one job's lines, in the order they ran.
//
// A run has MANY `test result:` lines — one per test binary — and they must never be
// summed: #2239's ledger figures (593/1, 588/6, 590/4, 592/2) are the `loomux_engine` lib
// target alone, out of a run whose targets total in the thousands. Attribution is by the
// `Running` line that precedes each result, which is why `stripAnsi` above is load-bearing.
//
// `ranTargets` (assembled by the caller) is reported alongside because `cargo test` STOPS
// at the first failing test binary (`ci-validate`): a round that reddens the engine lib
// means the src-tauri integration binaries never executed at all, and a reader who assumed
// "the rest of the suite passed in the same run" would be wrong. The list is the answer.
// A `Running` banner is NOT "the target of the lines that follow it", and reading it that way
// is a silent mis-attribution. Cargo prints the banner on STDERR while the test binary prints
// its own output on STDOUT, and GitHub's log is the two streams interleaved by arrival. Two
// banners therefore land back to back while the FIRST binary's trailing lines are still in
// flight — measured on run 33937535584's ubuntu leg, where `Running … loomux_server-…/main.rs`
// and `Running … loomux_lib-…/lib.rs` are adjacent and the `test result: ok. 25 passed` that
// follows them belongs to `loomux_server`'s LIB, two banners back. Read as "the last banner
// wins" that run reports `loomux_lib` at 25 instead of 280 and loses a target entirely, with
// nothing to say so: 49 targets against the same tree's 50.
//
// So the banners are a QUEUE, not a cursor. Cargo runs test binaries sequentially, so banner
// order is result order — interleaving reorders lines ACROSS the two streams, never within
// one — and each `test result:` pops the oldest banner that has not been resolved yet.
// Failure blocks accumulate the same way: they are stdout, in order, ahead of their own
// binary's result, so the ones seen since the last result belong to the suite this result
// closes even when another binary's banner arrived in between.
//
// `running N tests` is queued too, and carried per suite as `announced`, so the arithmetic is
// checkable: it counts passed + failed + ignored. A result whose queue is EMPTY is not
// attributed to a neighbour — it gets the target `(unattributed)`, which `selectSuite` will
// name rather than fold into a real one.
//
// `ranTargets` (assembled by the caller) is reported alongside because `cargo test` STOPS
// at the first failing test binary (`ci-validate`): a round that reddens the engine lib
// means the src-tauri integration binaries never executed at all, and a reader who assumed
// "the rest of the suite passed in the same run" would be wrong. The list is the answer.
function readCargoSuites(entries) {
  const suites = [];
  const announced = [];
  const counts = [];
  let failures = [];
  let pendingName = null;
  let awaitingMessage = false;
  for (const e of entries) {
    const line = e.text;
    const run = line.match(RUNNING_RE);
    if (run) {
      announced.push({ target: targetStem(run[2]), what: run[1] });
      pendingName = null;
      awaitingMessage = false;
      continue;
    }
    const doc = line.match(DOCTESTS_RE);
    if (doc) {
      announced.push({ target: `doc-tests ${doc[1]}`, what: 'doc-tests' });
      pendingName = null;
      awaitingMessage = false;
      continue;
    }
    const count = line.match(RUNNING_COUNT_RE);
    if (count) { counts.push(Number(count[1])); continue; }

    const stdout = line.match(STDOUT_RE);
    if (stdout) {
      pendingName = stdout[1];
      awaitingMessage = false;
      failures.push({ name: pendingName, message: null });
      continue;
    }
    if (pendingName) {
      if (PANICKED_RE.test(line)) { awaitingMessage = true; continue; }
      if (awaitingMessage && line.trim() !== '') {
        failures[failures.length - 1].message = line.trim();
        pendingName = null;
        awaitingMessage = false;
        continue;
      }
      // A failure that never panics (a test returning `Err`) prints its reason directly,
      // with no `panicked at` header to key on.
      if (!awaitingMessage && /^(Error|error):/.test(line.trim())) {
        failures[failures.length - 1].message = line.trim();
        pendingName = null;
        continue;
      }
    }

    const res = line.match(RESULT_RE);
    if (res) {
      const who = announced.shift() || { target: '(unattributed)', what: 'no Running banner preceded this result' };
      const passed = Number(res[2]);
      const failed = Number(res[3]);
      const ignored = Number(res[4]);
      const announcedCount = counts.length ? counts.shift() : null;
      suites.push({
        kind: 'cargo',
        job: e.job,
        target: who.target,
        what: who.what,
        failures,
        ok: res[1] === 'ok',
        passed,
        failed,
        ignored,
        total: passed + failed,
        announced: announcedCount,
        countsReconcile: announcedCount === null ? null : announcedCount === passed + failed + ignored,
      });
      failures = [];
      pendingName = null;
      awaitingMessage = false;
    }
  }
  return suites;
}

const NODE_TESTS_RE = /^#\s+tests\s+(\d+)\s*$/;
const NODE_PASS_RE = /^#\s+pass\s+(\d+)\s*$/;
const NODE_FAIL_RE = /^#\s+fail\s+(\d+)\s*$/;
const NODE_SKIP_RE = /^#\s+skipped\s+(\d+)\s*$/;
const NODE_NOTOK_RE = /^\s*not ok\s+\d+\s+-\s+(.*)$/;

// `npm test` is `node --test`, whose non-TTY reporter is TAP. Its trailer is the only place
// a total is printed, and `# pass` EXCLUDES skips while `# tests` includes them — so the
// pass figure and the test count are different numbers and neither is derivable from the
// other. Both are carried, and `total` stays `passed + failed` so it means the same thing
// as the cargo one.
function readNodeSuites(entries) {
  const byJob = new Map();
  for (const e of entries) {
    const line = e.text;
    let s = byJob.get(e.job);
    if (!s) {
      s = { kind: 'node', job: e.job, target: 'node', what: 'node --test', failures: [], passed: null, failed: null, tests: null, skipped: 0 };
      byJob.set(e.job, s);
    }
    const notok = line.match(NODE_NOTOK_RE);
    if (notok) { s.failures.push({ name: notok[1].trim(), message: null }); continue; }
    let m = line.match(NODE_TESTS_RE); if (m) { s.tests = Number(m[1]); continue; }
    m = line.match(NODE_PASS_RE); if (m) { s.passed = Number(m[1]); continue; }
    m = line.match(NODE_FAIL_RE); if (m) { s.failed = Number(m[1]); continue; }
    m = line.match(NODE_SKIP_RE); if (m) { s.skipped = Number(m[1]); continue; }
  }
  const out = [];
  for (const s of byJob.values()) {
    if (s.passed === null || s.failed === null) continue;
    s.ok = s.failed === 0;
    s.total = s.passed + s.failed;
    out.push(s);
  }
  return out;
}

// Every suite in one run's log, cargo and node alike, scoped per job.
function readSuites(text) {
  const entries = parseRunLog(text);
  const jobs = new Map();
  for (const e of entries) {
    if (!jobs.has(e.job)) jobs.set(e.job, []);
    jobs.get(e.job).push(e);
  }
  const suites = [];
  for (const rows of jobs.values()) {
    suites.push(...readCargoSuites(rows));
    suites.push(...readNodeSuites(rows));
  }
  return suites;
}

// How a suite is named when the script has to say which ones it found or could not tell
// apart. The `what` half is load-bearing, not decoration: a crate's lib unittests and its
// bin unittests both build to `deps/<crate>-<hash>`, so they share a target stem and differ
// only here — `loomux_server` is 25 tests as `unittests src/lib.rs` and 0 as `unittests
// src/main.rs` in the same job of the same run. Keyed on the stem alone the two collapse
// into one label, `selectSuite` sees a single match and silently returns whichever came
// first, and the ledger then carries one of the two figures with nothing to say the other
// existed. Measured on run 33932644347's ubuntu leg.
function suiteLabel(s) { return `${s.job || '(no job)'} / ${s.target} [${s.what}]`; }

// Pick the one suite a row's figures are about.
//
// THE ZERO CASE IS AN ERROR, and that is the whole positive control. A log that truncated,
// a workflow that printed no totals, a target name that no longer exists after a rename —
// all three produce "no suite", and all three would otherwise render as a ledger row with
// empty figures, which reads exactly like a round that reddened nothing. `selectSuite`
// throws instead, naming what it DID find so the caller can fix the selector.
function selectSuite(suites, opts) {
  const o = opts || {};
  let pool = suites;
  if (o.job) pool = pool.filter((s) => s.job === o.job);
  if (o.target) pool = pool.filter((s) => s.target === o.target);
  if (o.what) pool = pool.filter((s) => String(s.what).includes(o.what));
  if (!pool.length) {
    const uniq = [...new Set(suites.map(suiteLabel))];
    throw new Error(
      `no test totals parsed${o.target ? ` for target "${o.target}"` : ''}${o.what ? ` matching "${o.what}"` : ''}${o.job ? ` in job "${o.job}"` : ''}. `
      + (uniq.length
        ? `The log carries ${uniq.length} suite(s): ${uniq.slice(0, 12).join(', ')}${uniq.length > 12 ? ', …' : ''}`
        : 'The log carries NO `test result:` or `# pass` line at all — it is truncated, or the run never reached the test step.'),
    );
  }
  if (o.failingOnly) {
    const red = pool.filter((s) => !s.ok);
    if (red.length === 1) return red[0];
    if (red.length > 1) {
      throw new Error(`${red.length} suites failed (${red.map(suiteLabel).join(', ')}) — name one with --target/--job/--what so the row is attributable`);
    }
  }
  const uniq = [...new Set(pool.map(suiteLabel))];
  if (uniq.length > 1) throw new Error(`${uniq.length} suites match: ${uniq.join(', ')} — name one with --target/--job/--what`);
  return pool[0];
}

// ---------------------------------------------------------------------------
// Rendering helpers
// ---------------------------------------------------------------------------

// A quoted failure line goes into a markdown table cell inside a code span, so a backtick
// would end the span and a pipe would end the cell. Both are neutralized rather than
// dropped; the cut width is a presentation choice, stated in the design note.
function abridge(line, width) {
  const w = width || ABRIDGE;
  return String(line == null ? '' : line).replace(/`/g, "'").replace(/\s+/g, ' ').slice(0, w).replace(/\|/g, '\\|');
}

function short(sha) { return sha ? String(sha).slice(0, 8) : '(unknown)'; }

// ---------------------------------------------------------------------------
// GENERATE
// ---------------------------------------------------------------------------

// `logs` maps a run id to that run's log TEXT. Nothing here fetches anything, which is what
// lets the suite drive the whole generator over fixtures.
function buildLedger(spec, logs) {
  const sel = { target: spec.target, job: spec.job, what: spec.what };
  const notes = [];

  let base = null;
  if (spec.base && spec.base.run != null) {
    const text = logs[String(spec.base.run)];
    if (text == null) throw new Error(`no log supplied for the base run ${spec.base.run}`);
    const suite = selectSuite(readSuites(text), sel);
    base = { run: String(spec.base.run), suite, headSha: spec.base.headSha || null };
  }

  const rows = [];
  for (const r of spec.rows || []) {
    const id = String(r.run);
    const text = logs[id];
    if (text == null) throw new Error(`no log supplied for run ${id} (round ${r.n})`);
    const all = readSuites(text);
    const suite = selectSuite(all, Object.assign({}, sel, { failingOnly: !sel.target }));
    const names = suite.failures.map((f) => f.name);

    // The names are a CROSS-CHECK on the count, never its source: the count is the log's own
    // `failed` figure. They disagree when a log truncated mid-block, and that disagreement is
    // the thing worth saying out loud rather than resolving silently either way.
    if (names.length !== suite.failed) {
      notes.push(`round ${r.n}: the log names ${names.length} failing test(s) but its own summary says ${suite.failed} — the block is truncated, or a failure printed no \`---- … stdout ----\` header`);
    }
    if (base && suite.total !== base.suite.total) {
      notes.push(`round ${r.n}: total ${suite.total} does not reconcile with the banked green's ${base.suite.total} — the red is not attributable to this round's behaviour until you can say why (ci-validate, "Reconcile every round's test count against the banked green's")`);
    }
    // The binary's own `running N tests` against the summary it printed. These disagree when
    // the banner queue has been mis-fed — a log whose two streams interleaved in a shape this
    // reader does not model — which is a fact about the ATTRIBUTION rather than about the
    // round, and the one figure that would otherwise be wrong with nothing to say so.
    if (suite.countsReconcile === false) {
      notes.push(`round ${r.n}: \`${suite.target}\` announced ${suite.announced} tests but its summary reports ${suite.passed + suite.failed + suite.ignored} — the figures may be attributed to the wrong target; read the log around its \`Running\` banner`);
    }

    const firstMessage = suite.failures.map((f) => f.message).find((m) => m) || null;
    rows.push({
      n: r.n,
      behaviour: r.behaviour || r.label || '',
      headSha: r.headSha || null,
      cutFrom: r.cutFrom || null,
      run: id,
      conclusion: suite.ok ? 'green' : 'red',
      passed: suite.passed,
      failed: suite.failed,
      total: suite.total,
      target: suite.target,
      job: suite.job,
      names,
      message: firstMessage,
      ranTargets: [...new Set(all.filter((s) => s.job === suite.job).map((s) => s.target))],
    });
  }
  return { base, rows, notes };
}

function renderLedger(ledger, opts) {
  const o = opts || {};
  const width = o.abridge || ABRIDGE;
  const L = [];
  if (ledger.base) {
    const b = ledger.base.suite;
    L.push(`**The banked green each red counts against** is run ${ledger.base.run}${ledger.base.headSha ? ` @ \`${short(ledger.base.headSha)}\`` : ''}: `
      + `${b.passed} passed / ${b.failed} failed in \`${b.target}\`${b.job ? ` (${b.job})` : ''}, ${b.total} in the suite. `
      + "Every figure below is read from that round's own log rather than adjusted by arithmetic.");
    L.push('');
  }
  L.push('| # | behaviour set aside | scratch SHA | cut from | run | tests reddened | failure line (abridged) |');
  L.push('|---|---|---|---|---|---|---|');
  for (const r of ledger.rows) {
    const names = r.names.length
      ? r.names.map((n) => `\`${n}\``).join(', ')
      : '**none — a round that reddens NOTHING is a finding, not a row to drop**';
    L.push(`| ${r.n} | ${r.behaviour} | ${r.headSha ? `\`${short(r.headSha)}\`` : '(unknown)'} | ${r.cutFrom ? `\`${short(r.cutFrom)}\`` : '(unknown)'} | ${r.run} | ${names} | ${r.message ? `\`${abridge(r.message, width)}\`` : '(no failure line in the log)'} |`);
  }
  L.push('');
  L.push("Bullet form, one per round, each dated to the head SHA its own run reports:");
  L.push('');
  for (const r of ledger.rows) {
    const k = r.failed;
    L.push(`- **Round ${r.n} — ${k} test${k === 1 ? '' : 's'}** (${r.passed}/${r.failed}, run ${r.run} @ \`${short(r.headSha)}\`). `
      + (r.names.length ? r.names.map((n) => `\`${n}\``).join(', ') : 'nothing reddened.'));
  }
  if (ledger.notes.length) {
    L.push('');
    L.push('Notes the generator could not resolve on its own:');
    for (const n of ledger.notes) L.push(`- ${n}`);
  }
  return L.join('\n');
}

// ---------------------------------------------------------------------------
// CHECK
// ---------------------------------------------------------------------------

// Split a markdown body into its tables. A table is a run of contiguous lines beginning
// with `|`; the first is the header and the second the delimiter.
function readTables(body) {
  const lines = String(body).replace(/\r\n/g, '\n').split('\n');
  const tables = [];
  let cur = null;
  for (let i = 0; i < lines.length; i += 1) {
    const line = lines[i];
    if (/^\s*\|/.test(line)) {
      if (!cur) cur = { start: i + 1, rows: [] };
      cur.rows.push({ n: i + 1, cells: splitRow(line) });
    } else if (cur) { tables.push(cur); cur = null; }
  }
  if (cur) tables.push(cur);
  return tables;
}

// Split one `|`-delimited row, honouring `\|` inside a cell — the escape `abridge` writes,
// and the one a hand-written failure line quoting a closure (`|a| a == "--bare"`) needs.
function splitRow(line) {
  const s = String(line).trim().replace(/^\|/, '').replace(/\|$/, '');
  const cells = [];
  let cur = '';
  for (let i = 0; i < s.length; i += 1) {
    if (s[i] === '\\' && s[i + 1] === '|') { cur += '|'; i += 1; continue; }
    if (s[i] === '|') { cells.push(cur.trim()); cur = ''; continue; }
    cur += s[i];
  }
  cells.push(cur.trim());
  return cells;
}

const HEADER_RUN = /\brun\b/i;
const HEADER_REDDENED = /redden/i;
const HEADER_SHA = /\bsha\b/i;
const HEADER_NUM = /^#$|round/i;

// Find the ledger table: the one whose header names a run column AND a reddened column.
// Keyed on the HEADER's meaning rather than on the table's position, because a body carries
// several tables and their order is the author's.
function findLedgerTable(body) {
  for (const t of readTables(body)) {
    if (t.rows.length < 3) continue;
    const head = t.rows[0].cells;
    const runCol = head.findIndex((c) => HEADER_RUN.test(c));
    const redCol = head.findIndex((c) => HEADER_REDDENED.test(c));
    if (runCol < 0 || redCol < 0) continue;
    return {
      line: t.rows[0].n,
      header: head,
      runCol,
      redCol,
      shaCol: head.findIndex((c) => HEADER_SHA.test(c)),
      numCol: head.findIndex((c) => HEADER_NUM.test(c)),
      rows: t.rows.slice(2),
    };
  }
  return null;
}

function backticked(cell) {
  const out = [];
  const re = /`([^`]+)`/g;
  let m;
  while ((m = re.exec(String(cell)))) out.push(m[1].trim());
  return out;
}

// `**Round 13 — six tests** (588/6)` and `**Rounds 33 and 34 — two each** (592/2)`. The
// count is spelled as a WORD as often as a digit in real bodies (#2239 spells every one of
// them), and a checker that reads only digits silently passes every worded one — the
// vacuous shape CLAUDE.md's absence-control rule is about.
const BULLET_RE = /\*\*Rounds?\s+([0-9]+(?:(?:\s*,\s*|\s+and\s+)[0-9]+)*)\s*[—–-]\s*([0-9]+|[A-Za-z]+)\s+(?:tests?|each)\s*[.,;:]?\s*\*\*(?:[^\n]{0,20}?\((\d+)\s*\/\s*(\d+)\))?/g;

function readBullets(body) {
  const lines = String(body).replace(/\r\n/g, '\n').split('\n');
  const out = [];
  for (let i = 0; i < lines.length; i += 1) {
    BULLET_RE.lastIndex = 0;
    let m;
    while ((m = BULLET_RE.exec(lines[i]))) {
      const rounds = m[1].split(/\s*,\s*|\s+and\s+/).map((n) => Number(n));
      const raw = m[2].toLowerCase();
      const k = /^\d+$/.test(raw) ? Number(raw) : NUMBER_WORDS[raw];
      out.push({
        line: i + 1,
        rounds,
        k: k === undefined ? null : k,
        kRaw: m[2],
        passed: m[3] ? Number(m[3]) : null,
        failed: m[4] ? Number(m[4]) : null,
      });
    }
  }
  return out;
}

// Re-read a posted body's ledger against the logs of the runs it cites.
//
// `logs` is keyed by run id, exactly as in `buildLedger`; a run the caller could not fetch
// is simply absent and produces a CHECK, never a silent pass.
function checkBody(body, logs, opts) {
  const o = opts || {};
  const findings = [];
  const add = (severity, line, message, detail) => findings.push({ severity, line, message, detail: detail || null });

  const table = findLedgerTable(body);
  if (!table) {
    add('CHECK', 1, 'no ledger table found — no table in this body has both a `run` and a `tests reddened` column');
    return { findings, counts: tally(findings), rows: 0 };
  }

  const byRound = new Map();
  let checked = 0;
  for (const row of table.rows) {
    const cells = row.cells;
    const runCell = cells[table.runCol] || '';
    const idm = runCell.match(/\b(\d{8,})\b/);
    if (!idm) { add('CHECK', row.n, `no run id in the run column (${JSON.stringify(runCell.slice(0, 60))})`); continue; }
    const id = idm[1];
    const n = table.numCol >= 0 ? Number(String(cells[table.numCol]).replace(/[^0-9]/g, '')) : null;
    const text = logs[id];
    if (text == null) { add('CHECK', row.n, `run ${id}: no log available — not fetched, or the run's logs have expired`); continue; }

    let suite;
    try {
      suite = selectSuite(readSuites(text), { target: o.target, job: o.job, what: o.what, failingOnly: !o.target });
    } catch (err) {
      add('CHECK', row.n, `run ${id}: ${err.message}`);
      continue;
    }
    checked += 1;
    if (n != null && !Number.isNaN(n)) byRound.set(n, { suite, id, line: row.n });

    // The head SHA the run measured, against the SHA the row claims it measured. This is
    // the figure a re-cut or a rebase stales with nothing to say so.
    if (table.shaCol >= 0) {
      const claimed = (backticked(cells[table.shaCol])[0] || '').trim();
      const actual = o.runHeads ? o.runHeads[id] : null;
      if (claimed && actual) {
        if (actual.startsWith(claimed) || claimed.startsWith(actual)) add('OK', row.n, `run ${id}: head \`${claimed}\` is what the run reports`);
        else add('MISMATCH', row.n, `run ${id}: the row says head \`${claimed}\`, the run reports \`${short(actual)}\``);
      } else if (claimed) {
        add('CHECK', row.n, `run ${id}: head \`${claimed}\` not re-derived (no run metadata supplied)`);
      }
    }

    // The reddened set, as a SET: order is the author's, membership is the log's. Compared
    // on the LAST `::` segment, because a body spells a test as declared while the harness
    // prints it module-qualified — the same rule #2239's own body states for a test name.
    const claimedNames = backticked(cells[table.redCol]).filter((s) => /^[A-Za-z_][A-Za-z0-9_:]*$/.test(s));
    const actualNames = suite.failures.map((f) => f.name);
    const actualShort = new Set(actualNames.map(lastSegment));
    const claimedShort = new Set(claimedNames.map(lastSegment));
    const missing = [...actualShort].filter((x) => !claimedShort.has(x));
    const extra = [...claimedShort].filter((x) => !actualShort.has(x));
    if (!claimedNames.length && actualNames.length) {
      add('MISMATCH', row.n, `run ${id}: the row names no reddened test, the log names ${actualNames.length}`, actualNames.join(', '));
    } else if (missing.length || extra.length) {
      add('MISMATCH', row.n, `run ${id}: reddened set disagrees — ${missing.length} in the log and not the row, ${extra.length} in the row and not the log`,
        `log only: ${missing.join(', ') || '(none)'}\nrow only: ${extra.join(', ') || '(none)'}`);
    } else {
      add('OK', row.n, `run ${id}: ${actualNames.length} reddened name(s) match the log (${suite.passed} passed / ${suite.failed} failed in \`${suite.target}\`)`);
    }

    // The count from the log's own summary, against the number of names the row lists.
    if (claimedNames.length && claimedNames.length !== suite.failed) {
      add('MISMATCH', row.n, `run ${id}: the row lists ${claimedNames.length} test(s), the run's own \`test result:\` line says ${suite.failed} failed`);
    }
  }

  for (const b of readBullets(body)) {
    for (const n of b.rounds) {
      const hit = byRound.get(n);
      if (!hit) { add('CHECK', b.line, `bullet names round ${n}, which no table row numbers`); continue; }
      const s = hit.suite;
      if (b.k == null) add('CHECK', b.line, `round ${n}: could not read the bullet's count from ${JSON.stringify(b.kRaw)}`);
      else if (b.k !== s.failed) add('MISMATCH', b.line, `round ${n}: the bullet says ${b.kRaw} test(s), run ${hit.id} says ${s.failed} failed`);
      else add('OK', b.line, `round ${n}: bullet count ${b.k} matches run ${hit.id}`);
      if (b.passed != null) {
        if (b.passed !== s.passed || b.failed !== s.failed) add('MISMATCH', b.line, `round ${n}: the bullet says ${b.passed}/${b.failed}, run ${hit.id} says ${s.passed}/${s.failed}`);
        else add('OK', b.line, `round ${n}: bullet split ${b.passed}/${b.failed} matches run ${hit.id}`);
      }
    }
  }

  return { findings, counts: tally(findings), rows: checked };
}

function lastSegment(name) { return String(name).split('::').pop(); }

function tally(findings) {
  const c = { MISMATCH: 0, CHECK: 0, OK: 0 };
  for (const f of findings) c[f.severity] = (c[f.severity] || 0) + 1;
  return c;
}

function renderCheck(result, ctx) {
  const L = [];
  L.push(`mutation-ledger --check — PR #${ctx.pr}: ${result.rows} ledger row(s) re-read from their runs' logs`);
  L.push('');
  for (const f of result.findings) {
    if (ctx.quiet && f.severity === 'OK') continue;
    L.push(`${f.severity.padEnd(8)} L${String(f.line).padStart(4)}  ${f.message}`);
    if (f.detail) for (const d of String(f.detail).split('\n')) L.push(`${' '.repeat(15)}| ${d.slice(0, 200)}`);
  }
  L.push('');
  L.push(`SUMMARY mutation-ledger --check #${ctx.pr}: ${result.counts.MISMATCH} MISMATCH, ${result.counts.CHECK} CHECK, ${result.counts.OK} OK`);
  return L.join('\n');
}

// ---------------------------------------------------------------------------
// I/O — the only impure part
// ---------------------------------------------------------------------------

function sh(cmd, args, o) {
  return execFileSync(cmd, args, Object.assign({ encoding: 'utf8', maxBuffer: 512 * 1024 * 1024, stdio: ['ignore', 'pipe', 'pipe'] }, o || {}));
}
function shq(cmd, args, o) {
  try { return { ok: true, out: sh(cmd, args, o) }; }
  catch (err) { return { ok: false, out: String((err && err.stdout) || ''), err: String((err && err.stderr) || (err && err.message) || '') }; }
}

// A run's log is a multi-megabyte zip download, and a 34-row ledger re-reads the same runs
// on every draft. The cache is keyed by run id alone: a finished run's log is immutable, so
// there is nothing to invalidate.
function fetchLog(id, o) {
  const opts = o || {};
  const dir = opts.cache || path.join('.scratch', 'mutation-ledger-logs');
  const file = path.join(dir, `${id}.log`);
  if (fs.existsSync(file)) return fs.readFileSync(file, 'utf8');
  const args = ['run', 'view', String(id), '--log'];
  if (opts.repo) args.push('--repo', opts.repo);
  const r = shq('gh', args);
  // A FAILED run exits non-zero from `gh run view --log` while still printing the whole
  // log, so a non-zero exit with output is the normal case here, not an error.
  if (!r.ok && !r.out) throw new Error(`could not read the log of run ${id}: ${String(r.err).split('\n')[0]}`);
  fs.mkdirSync(dir, { recursive: true });
  fs.writeFileSync(file, r.out);
  return r.out;
}

function fetchRunHead(id, o) {
  const args = ['run', 'view', String(id), '--json', 'headSha,conclusion'];
  if (o && o.repo) args.push('--repo', o.repo);
  const r = shq('gh', args);
  if (!r.ok) return null;
  try { return JSON.parse(r.out).headSha; } catch (e) { return null; }
}

// ---------------------------------------------------------------------------
// CLI
// ---------------------------------------------------------------------------

function parseArgs(argv) {
  const o = { rows: null, check: null, repo: null, target: null, job: null, what: null, cache: null, abridge: null, json: false, quiet: false, help: false, bodyFile: null, logDir: null };
  for (let i = 0; i < argv.length; i += 1) {
    const a = argv[i];
    if (a === '--rows') o.rows = argv[++i];
    else if (a === '--check') o.check = Number(argv[++i]);
    else if (a === '--repo') o.repo = argv[++i];
    else if (a === '--target') o.target = argv[++i];
    else if (a === '--job') o.job = argv[++i];
    else if (a === '--what') o.what = argv[++i];
    else if (a === '--cache') o.cache = argv[++i];
    else if (a === '--abridge') o.abridge = Number(argv[++i]);
    else if (a === '--body-file') o.bodyFile = argv[++i];
    else if (a === '--log-dir') o.logDir = argv[++i];
    else if (a === '--json') o.json = true;
    else if (a === '--quiet') o.quiet = true;
    else if (a === '--help' || a === '-h') o.help = true;
    else throw new Error(`unknown argument: ${a}`);
  }
  return o;
}

const USAGE = `mutation-ledger — build the red-before-green ledger from the runs' own logs (#2507)

  node scripts/mutation-ledger.cjs --rows rows.json [--target loomux_engine] [--job "build (ubuntu-22.04)"]
                                   [--what 'src/lib.rs']   # tells a crate's lib unittests from its bin's
  node scripts/mutation-ledger.cjs --check <pr> [--repo owner/name] [--quiet]

  --log-dir <d>   read <run>.log from a directory instead of calling gh (offline)
  --cache <d>     where fetched logs are kept (default .scratch/mutation-ledger-logs)
  --body-file <f> check a body from a file instead of \`gh pr view\`

rows.json:
  { "target": "loomux_engine", "job": "build (ubuntu-22.04)",
    "base": { "run": 33932644347 },
    "rows": [ { "n": 1, "behaviour": "…", "run": 33928049423, "cutFrom": "7b36ae86" } ] }

Every figure is READ from a log, never derived: the suite total is one \`test result:\`
line's own passed+failed, and a run whose log yields no totals is an error rather than an
empty row. Exits 0 always — a report, not a gate. MISMATCH must be zero before report(done).
Seam: \`pr-body-check\` re-measures a body against HEAD and never opens a log; this opens a
log and never looks at head. Run both.`;

function loadLogs(ids, o) {
  const logs = {};
  for (const id of ids) {
    const key = String(id);
    if (logs[key] !== undefined) continue;
    if (o.logDir) {
      const f = path.join(o.logDir, `${key}.log`);
      if (fs.existsSync(f)) logs[key] = fs.readFileSync(f, 'utf8');
      continue;
    }
    try { logs[key] = fetchLog(key, o); }
    catch (err) { process.stderr.write(`mutation-ledger: ${err.message}\n`); }
  }
  return logs;
}

function main(argv) {
  const o = parseArgs(argv);
  if (o.help || (!o.rows && !o.check)) { process.stdout.write(`${USAGE}\n`); return 0; }

  if (o.rows) {
    const spec = JSON.parse(fs.readFileSync(o.rows, 'utf8'));
    if (o.target) spec.target = o.target;
    if (o.job) spec.job = o.job;
    if (o.what) spec.what = o.what;
    const ids = (spec.rows || []).map((r) => r.run);
    if (spec.base && spec.base.run != null) ids.push(spec.base.run);
    const logs = loadLogs(ids, o);
    // The head SHA is the RUN's own metadata, not the worker's memory of it. Offline
    // (`--log-dir`) a declared `headSha` is used, and a row with neither prints `(unknown)`
    // rather than a guess.
    for (const r of spec.rows || []) if (!r.headSha && !o.logDir) r.headSha = fetchRunHead(r.run, o);
    if (spec.base && spec.base.run != null && !spec.base.headSha && !o.logDir) spec.base.headSha = fetchRunHead(spec.base.run, o);
    const ledger = buildLedger(spec, logs);
    if (o.json) process.stdout.write(`${JSON.stringify(ledger, null, 2)}\n`);
    else process.stdout.write(`${renderLedger(ledger, { abridge: o.abridge })}\n`);
    return 0;
  }

  const body = o.bodyFile
    ? fs.readFileSync(o.bodyFile, 'utf8')
    : sh('gh', ['pr', 'view', String(o.check), ...(o.repo ? ['--repo', o.repo] : []), '--json', 'body', '--jq', '.body']);
  const table = findLedgerTable(body);
  const ids = table
    ? [...new Set(table.rows.map((r) => (String(r.cells[table.runCol] || '').match(/\b(\d{8,})\b/) || [])[1]).filter(Boolean))]
    : [];
  const logs = loadLogs(ids, o);
  const runHeads = {};
  if (!o.logDir) for (const id of ids) runHeads[id] = fetchRunHead(id, o);
  const result = checkBody(body, logs, { target: o.target, job: o.job, what: o.what, runHeads });
  if (o.json) process.stdout.write(`${JSON.stringify(result, null, 2)}\n`);
  else process.stdout.write(`${renderCheck(result, { pr: o.check, quiet: o.quiet })}\n`);
  return 0;
}

module.exports = {
  stripAnsi, parseRunLog, targetStem, readCargoSuites, readNodeSuites, readSuites, selectSuite,
  suiteLabel, abridge, buildLedger, renderLedger, readTables, splitRow, findLedgerTable, readBullets,
  checkBody, renderCheck, parseArgs, fetchLog, loadLogs, main, USAGE, ABRIDGE, NUMBER_WORDS,
};

if (require.main === module) {
  // Exit 0 always — a report, never a gate. A crash in the reader must not read as a defect
  // in the ledger, so it goes to stderr and the exit code stays 0.
  try { process.exitCode = main(process.argv.slice(2)); }
  catch (err) { process.stderr.write(`mutation-ledger: ${(err && err.message) || String(err)}\n`); process.exitCode = 0; }
}

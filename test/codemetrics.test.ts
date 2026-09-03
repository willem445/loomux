// `scripts/code-metrics.cjs` — the code-metrics report and the per-PR delta (#2138).
//
// WHAT THIS PINS, and why each pin can fail. The script's whole value is that a
// number it prints is the number a reader would get by hand, so every counter is
// pinned against a synthetic corpus in `test/fixtures/codemetrics/` built so that no
// two counters share a value — a fixture whose axes are all one constant cannot tell
// a working counter from a broken one (#1182).
//
// The TS corpus is five modules, each carrying exactly one property under test:
//
//   a.ts / b.ts   import each other and nothing else — the ONLY strongly-connected
//                 component, so "one cycle, of exactly these two" is fail-able in
//                 both directions.
//   dead.ts       exports `usedByNobody` AND `usedByEntry`, identical in every way
//                 except that one has an importer. A dead-export check that reports
//                 both, or neither, fails here; a fixture carrying only the dead
//                 export could not distinguish them.
//   long.ts       `longFunction` is exactly 120 lines, three deep, two arguments —
//                 three different numbers on one function, so a row that reads the
//                 wrong field cannot pass. `shortFunction` beside it keeps the
//                 distribution from being a single value.
//   entry.ts      the consumer. It is imported by nothing, so its own export `run`
//                 IS reported dead — that is the entrypoint blind spot the design
//                 note discloses, pinned here rather than left as prose (CLAUDE.md,
//                 "a documented escape hatch is a counterfactual").
//
// The Rust corpus is two synthetic `cargo clippy --message-format=json` streams. No
// cargo runs here and none can: agents never build Rust locally, and the parser's
// contract is with clippy's message TEXT, which a fixture pins exactly as well as a
// real run would. `clippy-ubuntu.json` carries one message whose wording the parser
// cannot read — it must land in `unparsed` and produce NO row, because a missing
// row is honest and a zero is a wrong number.
//
// REPORT-ONLY is itself pinned: `buildDelta` is fed a missing base and a
// clippy-less report and must still produce a body.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { execFileSync } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const require_ = createRequire(import.meta.url);
const here = path.dirname(fileURLToPath(import.meta.url));
const repoRoot = path.resolve(here, '..');
const scriptPath = path.join(repoRoot, 'scripts', 'code-metrics.cjs');
const fixtures = path.join(here, 'fixtures', 'codemetrics');

const cm = require_(scriptPath) as any;

// `[]` for the consumer roots: the fixture corpus is closed, so a dead export in it
// is dead full stop. The real run passes `src/`'s consumers (`test/`, `e2e/`).
function analyzeFixtureTs() {
  return cm.analyzeTypeScript(path.join(fixtures, 'src'), fixtures, []);
}

// ---------------------------------------------------------------------------
// Distributions
// ---------------------------------------------------------------------------

test('percentile is nearest-rank, so every reported value is one a real subject has', () => {
  const s = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10];
  // ceil(p/100 * 10): p50 -> rank 5 -> 5; p90 -> rank 9 -> 9; p95 -> rank 10 -> 10.
  assert.equal(cm.percentile(s, 50), 5);
  assert.equal(cm.percentile(s, 90), 9);
  assert.equal(cm.percentile(s, 95), 10);
  // No interpolation: 5.5 would be nobody's value.
  assert.notEqual(cm.percentile(s, 50), 5.5);
  assert.equal(cm.percentile([], 50), null);
});

test('distribution reports n and max beside the percentiles, and n is the population it saw', () => {
  const d = cm.distribution([3, 1, 2, null, NaN, 'x']);
  assert.deepEqual(d, { n: 3, p50: 2, p90: 3, p95: 3, max: 3 });
});

// ---------------------------------------------------------------------------
// Line counting
// ---------------------------------------------------------------------------

test('a comment line is one whose first non-blank characters are //, and a trailing newline is not a line', () => {
  const c = cm.countLines(['// why', '  /// doc', 'code(); // trailing', '', 'more();', ''].join('\n'));
  assert.deepEqual(c, { lines: 5, comment: 2, blank: 1, code: 2 });
});

// ---------------------------------------------------------------------------
// TypeScript
// ---------------------------------------------------------------------------

test('the cycle census reports exactly one component, of exactly the two modules that import each other', () => {
  const r = analyzeFixtureTs();
  // Positive control: the walk really ran over the whole corpus. Without it every
  // assertion below would also pass against an empty scan (#1209).
  assert.equal(r.available, true);
  assert.equal(r.files.length, 5);
  assert.equal(r.functions.length, 5);

  assert.deepEqual(r.cycles, [['src/a.ts', 'src/b.ts']]);
});

test('a self-import is a cycle, and an acyclic graph reports none', () => {
  assert.deepEqual(cm.stronglyConnected(['x', 'y'], [['x', 'y']]), []);
  assert.deepEqual(cm.stronglyConnected(['x'], [['x', 'x']]), [['x']]);
  assert.deepEqual(cm.stronglyConnected(['x', 'y', 'z'], [['x', 'y'], ['y', 'z'], ['z', 'x']]), [
    ['x', 'y', 'z'],
  ]);
});

test('an export with an importer is not dead and an otherwise identical one without is', () => {
  const r = analyzeFixtureTs();
  const dead = r.deadExports.map((d: any) => d.file + '#' + d.name).sort();
  // `usedByEntry` and `usedByNobody` are the same KIND of export in the same file;
  // only the importer differs. `entry.ts#run` is the disclosed entrypoint blind
  // spot — nothing imports the consumer, so its own export reads as dead.
  assert.deepEqual(dead, ['src/dead.ts#usedByNobody', 'src/entry.ts#run']);
  assert.ok(!dead.includes('src/dead.ts#usedByEntry'));
});

test('function length, nesting depth and argument count are three different reads of one function', () => {
  const r = analyzeFixtureTs();
  const long = r.functions.find((f: any) => f.name === 'longFunction');
  assert.ok(long, 'longFunction not found');
  assert.equal(long.lines, 120);
  assert.equal(long.depth, 3);
  assert.equal(long.args, 2);
  assert.equal(long.file, 'src/long.ts');

  const short = r.functions.find((f: any) => f.name === 'shortFunction');
  assert.equal(short.lines, 3);
  assert.equal(short.depth, 0);
  assert.equal(short.args, 0);

  // The distribution is fed by the same list, so max and p50 must differ.
  assert.equal(r.percentiles.fnLines.max, 120);
  assert.equal(r.percentiles.fnLines.p50, 3);
  assert.equal(r.percentiles.depth.max, 3);
  assert.equal(r.percentiles.args.max, 2);
});

test('fan-in and fan-out are counted per file from the resolved edges', () => {
  const r = analyzeFixtureTs();
  const by = new Map(r.files.map((f: any) => [f.file, f]));
  // entry imports three modules and is imported by none; a is imported by b and by
  // entry. Two different numbers on two different files, in both directions.
  assert.equal((by.get('src/entry.ts') as any).fanOut, 3);
  assert.equal((by.get('src/entry.ts') as any).fanIn, 0);
  assert.equal((by.get('src/a.ts') as any).fanIn, 2);
  assert.equal((by.get('src/a.ts') as any).fanOut, 1);
});

// ---------------------------------------------------------------------------
// Rust, through clippy's JSON
// ---------------------------------------------------------------------------

function ubuntuLeg() {
  return cm.parseClippyText(
    fs.readFileSync(path.join(fixtures, 'clippy-ubuntu.json'), 'utf8'),
    'ubuntu-22.04',
    fixtures
  );
}

test('three lints on one span merge into one function row carrying three distinct numbers', () => {
  const leg = ubuntuLeg();
  // Positive control: the stream really was read.
  assert.equal(leg.messagesSeen, 12);
  assert.equal(leg.parsed, 10);
  // Every coded diagnostic is accounted for exactly once. Without this, a lint
  // this report does not read would leave `parsed` short of `messagesSeen` and
  // be indistinguishable from a parser that stopped matching.
  assert.equal(leg.ignored, 1);
  assert.equal(leg.messagesSeen, leg.parsed + leg.ignored + leg.unparsed.length);

  const big = leg.functions.find((f: any) => f.name === 'big_fn');
  assert.ok(big, 'big_fn not found');
  assert.equal(big.lines, 140);
  assert.equal(big.cognitive, 31);
  assert.equal(big.args, 9);
});

test('a function name is read out of the source past its doc comment and attributes', () => {
  const leg = ubuntuLeg();
  // The span starts on `/// Doc.`; the `fn` line is two lines below it.
  const attributed = leg.functions.find((f: any) => f.line === 10);
  assert.equal(attributed.name, 'attributed_fn');
  assert.equal(attributed.lines, 4);
  // A lint that did not fire leaves null, never 0 — 0 is a measurement.
  assert.equal(attributed.cognitive, null);
  assert.equal(attributed.args, null);
});

test('a message whose value cannot be read produces no row and is disclosed, never a zero', () => {
  const leg = ubuntuLeg();
  assert.equal(leg.unparsed.length, 1);
  assert.match(leg.unparsed[0].message, /rather long indeed/);
  // rust/other.rs has unwrap/expect/panic counts but no function row, because the
  // one too_many_lines message naming it was unreadable.
  assert.equal(leg.functions.filter((f: any) => f.file === 'rust/other.rs').length, 0);
  assert.deepEqual(
    leg.perFile.find((r: any) => r.file === 'rust/other.rs'),
    {
      file: 'rust/other.rs',
      unwrap: 1,
      expect: 1,
      panic: 1,
      // No `column_start` on these fixture spans, so the column defaults to 0 — a
      // site is still one site, and the line still identifies it.
      sites: { unwrap: ['9:0'], expect: ['11:0'], panic: ['13:0'] },
    }
  );
  // Distinct counts per file, so a row cannot be read off the wrong file.
  assert.deepEqual(
    leg.perFile.find((r: any) => r.file === 'rust/lib.rs'),
    {
      file: 'rust/lib.rs',
      // TWO messages at line 5, at columns 9 and 30 — the `a.unwrap().b.unwrap()`
      // shape. A site is `line:column`, so these are two sites; keying on the line
      // alone would have reported one and under-counted every such line in the tree.
      unwrap: 2,
      expect: 0,
      panic: 0,
      sites: { unwrap: ['5:9', '5:30'], expect: [], panic: [] },
    }
  );
});

test('non-diagnostic cargo lines and non-JSON noise are skipped rather than counted', () => {
  // The fixture carries a `compiler-artifact` line, a `build-finished` line and one
  // line of plain text; messagesSeen stays at the twelve diagnostics.
  assert.equal(ubuntuLeg().messagesSeen, 12);
});

test('merging the legs keeps the larger value and the functions only one platform can see', () => {
  const ubuntu = ubuntuLeg();
  const windows = cm.parseClippyText(
    fs.readFileSync(path.join(fixtures, 'clippy-windows.json'), 'utf8'),
    'windows-latest',
    fixtures
  );
  const merged = cm.mergeClippy([ubuntu, windows]);
  assert.equal(merged.available, true);
  assert.deepEqual(merged.platforms, ['ubuntu-22.04', 'windows-latest']);

  // big_fn is 140 lines on ubuntu and 186 on windows (a cfg-gated body). The union
  // keeps 186 — under-reporting it is exactly what running clippy on one leg would
  // have done.
  const big = merged.functions.find((f: any) => f.name === 'big_fn');
  assert.equal(big.lines, 186);
  // ...and the other two numbers, which only the ubuntu leg carried, survive.
  assert.equal(big.cognitive, 31);
  assert.equal(big.args, 9);

  // A file only the windows leg saw is present at all.
  assert.ok(merged.functions.some((f: any) => f.file === 'rust/win.rs'));
  assert.ok(merged.perFile.some((r: any) => r.file === 'rust/win.rs'));

  // lib.rs 2 (two columns on one line) + other.rs 1 + win.rs 1 + split.rs 2 (the
  // disjoint cross-leg pair the union test above is built on).
  assert.equal(merged.totals.unwrap, 2 + 1 + 1 + 2);
  assert.equal(merged.totals.expect, 1);
  assert.equal(merged.totals.panic, 1);
  // Every leg carried sites, so the union is exact rather than a floor.
  assert.equal(merged.sitesComplete, true);
  assert.equal(merged.percentiles.fnLines.max, 186);
});

test('no clippy leg at all is a report with the Rust half marked unavailable, not a crash', () => {
  const merged = cm.mergeClippy([]);
  assert.equal(merged.available, false);
  assert.deepEqual(merged.functions, []);
  assert.equal(merged.percentiles.fnLines.n, 0);
  assert.equal(merged.percentiles.fnLines.p95, null);
});

// Review finding 3 (#2139 round 1): per-file `unwrap`/`expect`/`panic` counts were
// merged across legs with `Math.max`, while the design note claimed the merge means
// a `cfg`-gated body "is never under-reported". `max` is the union only when one
// leg's site set is a SUBSET of the other's. A file holding one `cfg(windows)` and
// one `cfg(unix)` unwrap reports 1 on each leg and merges to 1, not 2.
//
// The fixture is exactly that: `rust/split.rs` has its unwrap at line 10 on ubuntu
// and at line 40 on windows — same count per leg, disjoint sites, so a `max` merge
// and a union merge give DIFFERENT answers and the assertion can fail. A fixture
// where one leg simply had more sites would pass under both.

test('per-file unwrap counts merge as a union of SITES, not as the larger count', () => {
  const ubuntu = cm.parseClippyText(
    fs.readFileSync(path.join(fixtures, 'clippy-ubuntu.json'), 'utf8'),
    'ubuntu-22.04',
    fixtures
  );
  const windows = cm.parseClippyText(
    fs.readFileSync(path.join(fixtures, 'clippy-windows.json'), 'utf8'),
    'windows-latest',
    fixtures
  );

  // Positive control: the two legs really do disagree in the way the fixture is
  // built for — one site each, at different lines.
  const uSplit = ubuntu.perFile.find((r: any) => r.file === 'rust/split.rs');
  const wSplit = windows.perFile.find((r: any) => r.file === 'rust/split.rs');
  assert.equal(uSplit.unwrap, 1);
  assert.equal(wSplit.unwrap, 1);
  assert.deepEqual(uSplit.sites.unwrap, ['10:0']);
  assert.deepEqual(wSplit.sites.unwrap, ['40:0']);

  const merged = cm.mergeClippy([ubuntu, windows]);
  const m = merged.perFile.find((r: any) => r.file === 'rust/split.rs');
  // A `max` merge would say 1 here and silently drop the platform-specific site
  // that running clippy on every leg exists to catch.
  assert.equal(m.unwrap, 2);
  assert.deepEqual(m.sites.unwrap, ['10:0', '40:0']);
});

test('a site seen on every leg is counted once, not once per leg', () => {
  // The other direction, and the reason this is a union and not a sum: a plain
  // `unwrap` outside any `cfg` fires on all three legs and must still be one site.
  const leg = (platform: string) =>
    cm.parseClippyText(
      JSON.stringify({
        reason: 'compiler-message',
        message: {
          code: { code: 'clippy::unwrap_used' },
          message: 'used `unwrap()` on a `Result` value',
          spans: [{ file_name: 'rust/shared.rs', line_start: 7, line_end: 7, is_primary: true }],
        },
      }),
      platform,
      fixtures
    );
  const merged = cm.mergeClippy([leg('a'), leg('b'), leg('c')]);
  const row = merged.perFile.find((r: any) => r.file === 'rust/shared.rs');
  assert.equal(row.unwrap, 1);
  assert.equal(merged.totals.unwrap, 1);
});

test('a leg file with no site list degrades to its counts rather than throwing', () => {
  // An artifact written before sites were recorded (or by a future writer that drops
  // them) must not crash the merge. It cannot contribute to a union, so its counts
  // are taken as-is — the honest floor — and `sitesComplete` says the union is
  // partial rather than letting a reader believe otherwise.
  const legacy = { platform: 'old', functions: [], perFile: [{ file: 'rust/x.rs', unwrap: 5, expect: 0, panic: 0 }] };
  const merged = cm.mergeClippy([legacy]);
  const row = merged.perFile.find((r: any) => r.file === 'rust/x.rs');
  assert.equal(row.unwrap, 5);
  assert.equal(merged.sitesComplete, false);
});

// ---------------------------------------------------------------------------
// The diff view
// ---------------------------------------------------------------------------

const DIFF = [
  'diff --git a/src-tauri/src/thing.rs b/src-tauri/src/thing.rs',
  '--- a/src-tauri/src/thing.rs',
  '+++ b/src-tauri/src/thing.rs',
  '@@ -10,3 +10,6 @@',
  ' fn keep() {}',
  '-let gone = old.unwrap();',
  '+// why this is safe',
  '+let a = thing.unwrap();',
  '+let b = thing.expect("reason");',
  'diff --git a/src-tauri/tests/orchestration.rs b/src-tauri/tests/orchestration.rs',
  '--- a/src-tauri/tests/orchestration.rs',
  '+++ b/src-tauri/tests/orchestration.rs',
  '@@ -1,0 +1,2 @@',
  '+let t = fixture.unwrap();',
  '+assert!(t);',
].join('\n');

test('the added-lines view counts additions only, and only product Rust carries an unwrap row', () => {
  const d = cm.analyzeDiff(DIFF);
  assert.equal(d.files, 2);
  // Five `+` lines across the two files; the one `-` line is not an addition.
  assert.equal(d.addedLines, 5);
  assert.equal(d.addedComment, 1);
  assert.equal(d.commentShare, 0.2);

  // The test-file `.unwrap()` is an addition too, and must NOT be reported: the row
  // is about product code. Without the second file this assertion would pass under
  // an implementation that counted every root.
  assert.equal(d.addedUnwrap.length, 1);
  assert.equal(d.addedUnwrap[0].file, 'src-tauri/src/thing.rs');
  assert.equal(d.addedExpect.length, 1);
  assert.equal(d.addedPanic.length, 0);
  assert.equal(d.addedAllow.length, 0);
});

test('the added-lines view of an empty diff is zeros, not a throw', () => {
  const d = cm.analyzeDiff('');
  assert.equal(d.addedLines, 0);
  assert.equal(d.commentShare, 0);
});

// Review finding 2 (#2139 round 1): a diff whose ADDED CONTENT is itself a line
// beginning with `+++ ` was consumed as a file header. `files` inflated by one, the
// line never counted as added, and every later added line attributed to the bogus
// path — which, after the `b/` strip, can read as product Rust and misdirect the
// unwrap rows.
//
// The fixture is a docs file, because that is the realistic subject: a design note or
// a skill quoting a diff. It carries BOTH a `--- ` and a `+++ ` content line, so a
// fix that only special-cases one of them still fails.

const DIFF_QUOTING_A_DIFF = [
  'diff --git a/doc/design/example.md b/doc/design/example.md',
  '--- a/doc/design/example.md',
  '+++ b/doc/design/example.md',
  '@@ -1,0 +1,5 @@',
  '+Worked example of a diff:',
  '+',
  '+```diff',
  '+--- a/src-tauri/src/evil.rs',
  '+++ b/src-tauri/src/evil.rs',
].join('\n');

test('a diff whose added content is itself a `+++ ` line does not start a new file', () => {
  const d = cm.analyzeDiff(DIFF_QUOTING_A_DIFF);
  // One real file, not two: the `+++ b/src-tauri/src/evil.rs` inside the hunk is
  // content, not a header.
  assert.equal(d.files, 1);
  // Five `+` lines in the hunk, all of them added lines of the docs file.
  assert.equal(d.addedLines, 5);
  // And nothing is attributed to the path that only ever appeared inside a code
  // fence. Without this, `evil.rs` reads as product Rust and owns every later row.
  assert.equal(d.addedUnwrap.length, 0);
  assert.equal(d.addedExpect.length, 0);
});

test('a hunk that adds a line starting with `-- ` still counts it as an addition', () => {
  // The mirror case: `--- ` inside a hunk is a DELETED line whose content starts
  // `-- `, and must not be mistaken for a header either. Here the deleted line is
  // real, so the added count must exclude it and include only the two `+` lines.
  const d = cm.analyzeDiff(
    [
      'diff --git a/notes.md b/notes.md',
      '--- a/notes.md',
      '+++ b/notes.md',
      '@@ -1,2 +1,2 @@',
      '--- old header line',
      '+++ new header line',
      '+trailing',
    ].join('\n')
  );
  assert.equal(d.files, 1);
  assert.equal(d.addedLines, 2);
});

test('a plain unified diff with no `diff --git` line still resolves its file', () => {
  // `analyzeDiff` is fed `git diff`, which always emits `diff --git`. But the header
  // rule must not DEPEND on it, or a hand-made `diff -u` silently measures nothing.
  const d = cm.analyzeDiff(
    ['--- a/src-tauri/src/thing.rs', '+++ b/src-tauri/src/thing.rs', '@@ -1,0 +1,1 @@', '+let a = b.unwrap();'].join('\n')
  );
  assert.equal(d.files, 1);
  assert.equal(d.addedLines, 1);
  assert.equal(d.addedUnwrap.length, 1);
  assert.equal(d.addedUnwrap[0].file, 'src-tauri/src/thing.rs');
});

// ---------------------------------------------------------------------------
// The report and the delta comment
// ---------------------------------------------------------------------------

function fixtureReport(overrides: any = {}) {
  const base = {
    schemaVersion: cm.SCHEMA_VERSION,
    commit: 'basesha',
    ts: analyzeFixtureTs(),
    rust: cm.mergeClippy([ubuntuLeg()]),
    roots: {},
    modRs: { file: 'src-tauri/src/orchestration/mod.rs', lines: 1000 },
    diff: null,
  };
  return Object.assign(base, overrides);
}

test('the delta comment leads with the sticky marker, so the workflow can find and edit it', () => {
  const body = cm.buildDelta(fixtureReport(), fixtureReport({ commit: 'headsha' }), {});
  assert.equal(body.split('\n')[0], cm.COMMENT_MARKER);
  assert.equal(cm.COMMENT_MARKER, '<!-- code-metrics -->');
});

test('a new function above the base p95 is named; one that existed at the base is not', () => {
  const base = fixtureReport();
  const head = fixtureReport({ commit: 'headsha' });
  // Base p95 of TS function lines is 120 (longFunction). Add one new function above
  // it, and re-state an EXISTING one at a bigger size: only the new one is a row,
  // which is what "keyed on NEW entities, so existing debt never blocks" means.
  head.ts = JSON.parse(JSON.stringify(head.ts));
  head.ts.functions.push({ file: 'src/new.ts', name: 'freshGiant', line: 1, endLine: 200, lines: 200, depth: 1, args: 1 });
  const grown = head.ts.functions.find((f: any) => f.name === 'longFunction');
  grown.lines = 500;

  const named = cm.newFunctionsOverP95(base.ts.functions, head.ts.functions, base.ts.percentiles.fnLines.p95, 'lines');
  assert.deepEqual(named.map((f: any) => f.name), ['freshGiant']);

  const body = cm.buildDelta(base, head, {});
  assert.match(body, /freshGiant/);
  assert.ok(!/\| TS \| `longFunction`/.test(body), 'a pre-existing function must not be listed as new');
});

test('a cycle present at head and absent at base is reported as new; an unchanged set is not', () => {
  const base = fixtureReport();
  const head = fixtureReport({ commit: 'headsha' });
  const same = cm.buildDelta(base, head, {});
  assert.match(same, /### New TypeScript import cycles\n\nNone\./);

  head.ts = JSON.parse(JSON.stringify(head.ts));
  head.ts.cycles.push(['src/p.ts', 'src/q.ts']);
  const grew = cm.buildDelta(base, head, {});
  assert.match(grew, /- src\/p\.ts ↔ src\/q\.ts/);
});

test('a missing base degrades to "unavailable" and claims no delta — B never fails on a missing base', () => {
  const head = fixtureReport({ commit: 'headsha', diff: cm.analyzeDiff(DIFF) });
  const body = cm.buildDelta(null, head, { baseSha: 'deadbeef', baseNote: 'no artifact for the base run' });
  assert.match(body, /base could not be measured/);
  assert.match(body, /no artifact for the base run/);
  // Head figures still appear...
  assert.match(body, /Added lines \| 5/);
  // ...and no invented base number does.
  assert.match(body, /### New functions above the base p95\n\nBase unavailable/);
  assert.match(body, /### New TypeScript import cycles\n\nBase unavailable/);
});

test('a base with no clippy figures says so on the Rust rows rather than guessing a zero', () => {
  const base = fixtureReport({ rust: cm.mergeClippy([]) });
  const head = fixtureReport({ commit: 'headsha' });
  const body = cm.buildDelta(base, head, { baseNote: 'the base run kept no clippy artifact' });
  assert.match(body, /Base clippy figures unavailable/);
  // Every cell of every Rust row reads `n/a → <head>`, including `n`. A base with
  // no clippy leg has no population, so `0 → 2` would read as a measurement nobody
  // made — the row must be absent, not zero.
  assert.match(body, /\| Rust function CODE lines \(clippy\) \| n\/a → 2 \| n\/a → 4 \| n\/a → 140 \| n\/a → 140 \| n\/a → 140 \|/);
  assert.ok(!/\| Rust function CODE lines \(clippy\) \| 0 →/.test(body));
});

test('the delta comment says plainly that every row is report-only', () => {
  const body = cm.buildDelta(fixtureReport(), fixtureReport({ commit: 'headsha' }), {});
  assert.match(body, /report-only/i);
  assert.match(body, /cannot turn CI red/);
});

test('the mod.rs row carries base, head and the signed delta', () => {
  const base = fixtureReport();
  const head = fixtureReport({ commit: 'headsha', modRs: { file: 'src-tauri/src/orchestration/mod.rs', lines: 1191 } });
  const body = cm.buildDelta(base, head, {});
  assert.match(body, /mod\.rs` lines \| 1000 \| 1191 \| \+191 \|/);
});

// Review round 3 (#2139), blocking finding: `file#name` is not a function identity.
// Rust `impl` blocks routinely give one file many `fn as_str`, `fn new`, `fn fmt`,
// `fn drop` — `src-tauri/src/orchestration/mod.rs` declares 16 `as_str` alone, and
// the head artifact carried ONE row for them, publishing the maximum of their values
// at the first one's line. Measured on the head artifact: 2,441 rows under
// `file:line` collapse to 2,380 distinct `file#name` keys — 39 keys carry more than
// one row, 100 rows sit under those 39, and the 61 duplicates beyond the first row of
// each key are exactly what the old key deleted.
//
// Both fixtures below make the two operands COLLIDE, which the round-1 fixtures did
// not: they had no file with two same-named functions, so a `file#name` merge and a
// per-function merge produced identical output and the assertion held under both
// (CLAUDE.md #1182 / #1300 B1).

test('two same-named functions in one file stay two rows, with their own values', () => {
  const leg = {
    platform: 'ubuntu-22.04',
    messagesSeen: 2,
    parsed: 2,
    ignored: 0,
    unparsed: [],
    perFile: [],
    functions: [
      { file: 'src-tauri/src/x.rs', line: 10, endLine: 16, name: 'as_str', lines: 5, cognitive: 1, args: null },
      { file: 'src-tauri/src/x.rs', line: 900, endLine: 1400, name: 'as_str', lines: 480, cognitive: 70, args: null },
    ],
  };
  const m = cm.mergeClippy([leg]);

  // Two in, two out. Under a `file#name` key this is 1, and `n` below is 1.
  assert.equal(m.functions.length, 2);
  const small = m.functions.find((f: any) => f.line === 10);
  const big = m.functions.find((f: any) => f.line === 900);
  // Neither row may carry the other's numbers — the failure was the big function's
  // 480 lines being stamped on the 5-line one at line 10.
  assert.equal(small.lines, 5);
  assert.equal(small.cognitive, 1);
  assert.equal(big.lines, 480);
  assert.equal(big.cognitive, 70);
  // The population the percentiles run over is the real one.
  assert.equal(m.percentiles.fnLines.n, 2);
  assert.equal(m.percentiles.fnLines.max, 480);
});

test('one function reported by two legs is still one row, taking the larger value', () => {
  // The other direction, and the reason the key is `file:line` rather than anything
  // per-leg: a `cfg`-gated body genuinely is one function with two measurements.
  const leg = (platform: string, lines: number) => ({
    platform,
    messagesSeen: 1,
    parsed: 1,
    ignored: 0,
    unparsed: [],
    perFile: [],
    functions: [{ file: 'src-tauri/src/x.rs', line: 42, endLine: 99, name: 'gated', lines, cognitive: null, args: null }],
  });
  const m = cm.mergeClippy([leg('ubuntu-22.04', 20), leg('windows-latest', 55)]);
  assert.equal(m.functions.length, 1);
  assert.equal(m.functions[0].lines, 55);
  assert.equal(m.functions[0].name, 'gated');
});

test('a NEW same-named function above the base p95 is reported, and its sibling is not', () => {
  // The ratchet key. `newFunctionsOverP95` diffed base against head on `file#name`,
  // so adding a SECOND `render` to a file that already had one was invisible — the
  // key was already known. At head, 3,686 TS function rows collapse to 2,363 distinct
  // `file#name` keys: 133 keys carry more than one row and 1,323 rows are duplicates
  // beyond the first of their key — that is what the 1,323 counts, not "functions
  // sharing a name". 1,176 of them are `<anonymous>`; the 147 NAMED duplicates are
  // what this multiset recovers, and are the common case this test stands for.
  const base = [
    { file: 'src/a.ts', name: 'render', line: 10, lines: 10 },
    { file: 'src/a.ts', name: 'helper', line: 40, lines: 8 },
  ];
  const head = [
    { file: 'src/a.ts', name: 'render', line: 10, lines: 10 },
    { file: 'src/a.ts', name: 'helper', line: 40, lines: 8 },
    { file: 'src/a.ts', name: 'render', line: 200, lines: 600 },
  ];
  const out = cm.newFunctionsOverP95(base, head, 100, 'lines');
  assert.deepEqual(out.map((f: any) => f.line), [200]);
  assert.equal(out[0].lines, 600);
});

test('an EXISTING function that merely grew past the p95 is not reported as new', () => {
  // The discriminating half. A ratchet keyed on new ENTITIES must not fire when the
  // same function got bigger — that is existing debt, which #2128 part 7 is explicit
  // must never block. A count-only test would pass under an implementation that
  // reported every oversized function.
  const base = [{ file: 'src/a.ts', name: 'render', line: 10, lines: 10 }];
  const head = [{ file: 'src/a.ts', name: 'render', line: 10, lines: 600 }];
  assert.deepEqual(cm.newFunctionsOverP95(base, head, 100, 'lines'), []);
});

test('when several same-named siblings are added at once, the largest is the one named', () => {
  // Two new `render`s, one over the p95 and one under. Exactly one row, and it is the
  // one worth a reviewer's attention.
  const base = [{ file: 'src/a.ts', name: 'render', line: 10, lines: 10 }];
  const head = [
    { file: 'src/a.ts', name: 'render', line: 10, lines: 10 },
    { file: 'src/a.ts', name: 'render', line: 200, lines: 600 },
    { file: 'src/a.ts', name: 'render', line: 900, lines: 12 },
  ];
  const out = cm.newFunctionsOverP95(base, head, 100, 'lines');
  assert.deepEqual(out.map((f: any) => f.lines), [600]);
});

test('the delta subcommand degrades on an unreadable head report instead of throwing', () => {
  // `code-metrics.cjs`'s header promises "every parse error degrades to a missing
  // row". The head read was the one unguarded `JSON.parse` in the CLI, so a truncated
  // artifact threw out of the process — `continue-on-error` would have hidden it in
  // this workflow, which is exactly what makes the claim, not the behaviour, the
  // thing at risk (#2139 review round 3, premortem).
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'codemetrics-'));
  const head = path.join(dir, 'head.json');
  const out = path.join(dir, 'comment.md');
  try {
    fs.writeFileSync(head, '{"schemaVersion":1,"ts":{"percenti');
    execFileSync(process.execPath, [scriptPath, 'delta', '--head', head, '--out', out], {
      encoding: 'utf8',
      stdio: ['ignore', 'pipe', 'pipe'],
    });
    // Exit 0 — and an n/a COMMENT, not silence. Writing nothing would leave the
    // sticky comment from the previous run standing with its numbers, which a
    // reviewer reads as current; silently stale is worse than visibly absent.
    assert.equal(fs.existsSync(out), true);
    const body = fs.readFileSync(out, 'utf8');
    // Marker-led, so the workflow REPLACES the stale sticky comment rather than
    // posting beside it.
    assert.equal(body.split('\n')[0], cm.COMMENT_MARKER);
    assert.match(body, /unavailable for this run/);
    // It says why, and it says the job did not fail.
    assert.match(body, /Reason:/);
    assert.match(body, /nothing here gates the merge/);
    // And it carries NO figures: a number here would be from a run that is no longer
    // the head, which is the whole failure being degraded away from.
    assert.ok(!/\| Metric \|/.test(body));
    assert.ok(!/p95/.test(body));
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

// ---------------------------------------------------------------------------
// The CLI and the persisted schema
// ---------------------------------------------------------------------------

// The whole CI pipeline in one test: each leg's raw cargo stream through the
// `clippy` subcommand, then both compact leg files into `report`. `--clippy` takes
// the COMPACT form, never the raw stream — a report handed a raw stream would
// silently produce an empty Rust half, so running the real two-step is the pin.
test('the report subcommand writes the documented schema and a summary, and exits 0', () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'codemetrics-'));
  const out = path.join(dir, 'code-metrics.json');
  const summary = path.join(dir, 'summary.md');
  try {
    const legs = ['ubuntu-22.04', 'windows-latest'].map((platform, i) => {
      const legOut = path.join(dir, 'leg-' + i + '.json');
      execFileSync(
        process.execPath,
        [scriptPath, 'clippy', '--platform', platform, '--repo-root', fixtures, '--out', legOut],
        {
          input: fs.readFileSync(
            path.join(fixtures, i === 0 ? 'clippy-ubuntu.json' : 'clippy-windows.json'),
            'utf8'
          ),
          encoding: 'utf8',
        }
      );
      return legOut;
    });
    execFileSync(
      process.execPath,
      [
        scriptPath, 'report',
        '--repo-root', fixtures,
        '--clippy', legs[0],
        '--clippy', legs[1],
        '--commit', 'fixturesha',
        '--out', out,
        '--summary', summary,
      ],
      { encoding: 'utf8' }
    );
    const report = JSON.parse(fs.readFileSync(out, 'utf8'));
    // The schema B and the future scorecard column read. A renamed key here is a
    // break in a persisted contract, not a refactor (doc/design/code-metrics.md).
    assert.deepEqual(
      Object.keys(report).sort(),
      ['commit', 'diff', 'generatedAt', 'generator', 'modRs', 'ref', 'roots', 'rust', 'schemaVersion', 'ts'].sort()
    );
    assert.equal(report.schemaVersion, 1);
    assert.equal(report.commit, 'fixturesha');
    assert.deepEqual(report.rust.platforms, ['ubuntu-22.04', 'windows-latest']);
    assert.equal(report.ts.cycles.length, 1);
    // The fixture tree has no `src-tauri/`, so mod.rs is absent — a missing file is
    // a null, never a zero and never a throw.
    assert.equal(report.modRs.lines, null);

    const text = fs.readFileSync(summary, 'utf8');
    assert.match(text, /report-only/);
    assert.match(text, /longFunction/);
    assert.match(text, /big_fn/);
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

test('the clippy subcommand reads a cargo stream on stdin and writes the compact leg file', () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'codemetrics-'));
  const out = path.join(dir, 'leg.json');
  try {
    execFileSync(
      process.execPath,
      [scriptPath, 'clippy', '--platform', 'ubuntu-22.04', '--repo-root', fixtures, '--out', out],
      { input: fs.readFileSync(path.join(fixtures, 'clippy-ubuntu.json'), 'utf8'), encoding: 'utf8' }
    );
    const leg = JSON.parse(fs.readFileSync(out, 'utf8'));
    assert.equal(leg.platform, 'ubuntu-22.04');
    assert.equal(leg.functions.length, 2);
    assert.equal(leg.functions.find((f: any) => f.name === 'big_fn').cognitive, 31);
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

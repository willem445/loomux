// `test/mutationledger.test.ts` — the ledger generator's own pins (#2507).
//
// WHAT THIS SUITE IS FOR. `scripts/mutation-ledger.cjs` reads a red-before-green ledger out
// of the runs' own logs instead of a worker typing it. Its whole value is that the figures
// it prints are the figures GitHub printed, so every assertion here is anchored to a real
// excerpt (`test/fixtures/mutationledger/README.md` says which run each came from) rather
// than to a shape invented for the test.
//
// THE POSITIVE CONTROL IS THE POINT OF THE FILE. A parse that finds nothing renders exactly
// like a run that reddened nothing: both are an empty ledger. So `truncated.log` — a real
// log with its `test result:` line removed — must make the reader THROW, and that assertion
// is what stops every "the figures match" assertion below from being satisfiable by a
// reader that silently found no suites at all.
//
// Nothing here runs `gh` or touches the network: the pure core takes log TEXT.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const require_ = createRequire(import.meta.url);
const here = path.dirname(fileURLToPath(import.meta.url));
const root = path.resolve(here, '..');
const fixtures = path.join(here, 'fixtures', 'mutationledger');

const ml = require_(path.join(root, 'scripts', 'mutation-ledger.cjs')) as any;

const log = (name: string) => fs.readFileSync(path.join(fixtures, `${name}.log`), 'utf8');
const body = (name: string) => fs.readFileSync(path.join(fixtures, `body-${name}.md`), 'utf8');

type Suite = {
  kind: string; job: string; target: string; what: string; ok: boolean;
  passed: number; failed: number; total: number;
  failures: { name: string; message: string | null }[];
};
type Finding = { severity: string; line: number; message: string; detail: string | null };
type CheckResult = { findings: Finding[]; counts: Record<string, number>; rows: number };

const suitesOf = (name: string): Suite[] => ml.readSuites(log(name)) as Suite[];
const RUN_ROUND1 = '33928049423';
const RUN_ROUND13 = '33928069681';

// ---------------------------------------------------------------------------
// The positive control, first. Everything below asserts that some figure came back
// right; this asserts that "no figure at all" is loud.
// ---------------------------------------------------------------------------

test('a log with no totals in it is an ERROR, never an empty ledger', () => {
  const suites = suitesOf('truncated');
  assert.equal(suites.length, 0, 'the fixture must genuinely parse to no suite — otherwise the throw below is about something else');
  assert.throws(
    () => ml.selectSuite(suites, { target: 'loomux_engine' }),
    /no test totals parsed/,
    'a truncated log must throw rather than yield a row whose figures are blank',
  );
});

test('the zero-case error says the log carried NO totals at all, not merely none matching', () => {
  // The two diagnoses need different fixes — a truncated log is re-run, a wrong selector is
  // re-typed — and a reader handed one message for both cannot tell which they have.
  let truncatedMessage = '';
  try { ml.selectSuite(suitesOf('truncated'), { target: 'loomux_engine' }); } catch (e) { truncatedMessage = String((e as Error).message); }
  let wrongTargetMessage = '';
  try { ml.selectSuite(suitesOf('red-round13'), { target: 'a_target_that_never_existed' }); } catch (e) { wrongTargetMessage = String((e as Error).message); }

  assert.match(truncatedMessage, /NO `test result:` or `# pass` line at all/);
  assert.doesNotMatch(truncatedMessage, /The log carries \d+ suite/);
  assert.match(wrongTargetMessage, /The log carries 2 suite\(s\)/, 'the wrong-selector message must name what WAS found');
  assert.match(wrongTargetMessage, /loomux_engine/);
});

test('a ledger cannot be built at all from a log with no totals', () => {
  // The error has to survive the whole generate path, not only the selector: a `buildLedger`
  // that swallowed it would print a table with one blank row and exit 0.
  assert.throws(
    () => ml.buildLedger(
      { target: 'loomux_engine', rows: [{ n: 1, behaviour: 'x', run: RUN_ROUND1 }] },
      { [RUN_ROUND1]: log('truncated') },
    ),
    /no test totals parsed/,
  );
});

// ---------------------------------------------------------------------------
// Reading a cargo log
// ---------------------------------------------------------------------------

test('the figures come off the run’s own `test result:` line', () => {
  const s = ml.selectSuite(suitesOf('red-round13'), { target: 'loomux_engine' }) as Suite;
  assert.equal(s.passed, 588);
  assert.equal(s.failed, 6);
  assert.equal(s.total, 594, 'the total is passed+failed off ONE line, never a sum across targets');
  assert.equal(s.ok, false);
});

test('a run’s many `test result:` lines are attributed per target, never summed', () => {
  // The discriminating property: this fixture's six targets have six DIFFERENT totals, so a
  // reader that summed them, or took the first, or ignored the job would fail here.
  const ubuntu = suitesOf('green-multitarget').filter((s) => s.job === 'build (ubuntu-22.04)');
  const byTarget = new Map<string, number[]>();
  for (const s of ubuntu) byTarget.set(s.target, [...(byTarget.get(s.target) || []), s.total]);
  assert.deepEqual(byTarget.get('loomux_engine'), [594]);
  assert.deepEqual(byTarget.get('loomux_lib'), [280]);
  assert.deepEqual(byTarget.get('acl_manifest'), [3]);
  assert.deepEqual(byTarget.get('node'), [2810]);
  const sum = ubuntu.reduce((a, s) => a + s.total, 0);
  assert.notEqual(sum, 594, 'if the sum happened to equal the engine total the assertions above would not discriminate');
});

test('the job is part of a suite’s identity, so one leg’s figures are never another’s', () => {
  const suites = suitesOf('green-multitarget');
  const jobs = [...new Set(suites.map((s) => s.job))].sort();
  assert.deepEqual(jobs, ['build (macos-latest)', 'build (ubuntu-22.04)']);
  assert.throws(
    () => ml.selectSuite(suites, { target: 'loomux_engine' }),
    /suites match/,
    'two legs both carry a loomux_engine suite; picking one silently would date a figure to the wrong platform',
  );
  const one = ml.selectSuite(suites, { target: 'loomux_engine', job: 'build (macos-latest)' }) as Suite;
  assert.equal(one.job, 'build (macos-latest)');
  assert.equal(one.passed, 594);
});

test('two suites that share a target stem are told apart by `what`, not collapsed', () => {
  // `loomux_server`'s lib and bin unittests both build to `deps/loomux_server-<hash>`. Keyed
  // on the stem alone the 25-test suite and the 0-test one are one label, and the selector
  // returns whichever came first with nothing to say the other existed.
  const ubuntu = suitesOf('green-multitarget').filter((s) => s.job === 'build (ubuntu-22.04)' && s.target === 'loomux_server');
  assert.equal(ubuntu.length, 2, 'the fixture must carry both, or this test is about nothing');
  assert.deepEqual(ubuntu.map((s) => s.total).sort((a, b) => a - b), [0, 25]);
  assert.throws(
    () => ml.selectSuite(ubuntu, { target: 'loomux_server', job: 'build (ubuntu-22.04)' }),
    /suites match/,
  );
  const lib = ml.selectSuite(ubuntu, { target: 'loomux_server', job: 'build (ubuntu-22.04)', what: 'src/lib.rs' }) as Suite;
  assert.equal(lib.passed, 25);
});

test('the reddened set is the log’s, with each name’s own failure line beside it', () => {
  const s = ml.selectSuite(suitesOf('red-round13'), { target: 'loomux_engine' }) as Suite;
  assert.equal(s.failures.length, 6);
  assert.deepEqual(
    s.failures.map((f) => String(f.name).split('::').pop()).sort(),
    [
      'a_compact_boundary_carries_its_trigger_and_its_pre_token_count',
      'a_stream_event_delta_is_text_and_a_non_text_delta_is_not',
      'an_unknown_message_type_is_ignored_as_an_event_and_kept_as_evidence',
      'an_unrecognized_terminal_reason_is_carried_not_collapsed',
      'init_booted_result_is_the_decoder_walking_one_turn',
      'pump_publishes_events_and_logs_the_lines_it_could_not_decode',
    ],
  );
  assert.equal(s.failures.length, s.failed, 'the names and the summary must reconcile, or a row is quoting a truncated block');
  for (const f of s.failures) assert.ok(f.message, `${f.name} must carry the assertion line the panic printed`);
});

test('the failure line quoted is the assertion, not the `panicked at` header', () => {
  const s = ml.selectSuite(suitesOf('red-round1-esc'), { target: 'loomux_engine' }) as Suite;
  assert.equal(s.failures.length, 1);
  assert.equal(
    s.failures[0].message,
    'assertion failed: session_ids_match(minted, "550E8400-E29B-41D4-A716-446655440000")',
  );
  assert.doesNotMatch(String(s.failures[0].message), /panicked at/);
});

// ---------------------------------------------------------------------------
// The two spellings of ESC
// ---------------------------------------------------------------------------

test('a log carrying the real 0x1B byte and one carrying `^[` parse to the same figures', () => {
  const withEsc = log('red-round1-esc');
  assert.ok(withEsc.includes('\u001b'), 'this fixture must carry the real byte, or the pair below is one spelling twice');
  const withCaret = withEsc.split('\u001b').join('^[');
  assert.ok(!withCaret.includes('\u001b'));

  const a = ml.selectSuite(ml.readSuites(withEsc), { target: 'loomux_engine' }) as Suite;
  const b = ml.selectSuite(ml.readSuites(withCaret), { target: 'loomux_engine' }) as Suite;
  assert.deepEqual([a.passed, a.failed], [b.passed, b.failed]);
  assert.deepEqual(a.failures, b.failures);
  assert.equal(a.passed, 593);
});

test('a stripper that knew only 0x1B would find NO target in the caret spelling', () => {
  // The negative control for the pair above: without the caret arm the failure is silence,
  // not a wrong number, which is why it needs its own assertion rather than a shared one.
  const caret = log('red-round1-esc').split('\u001b').join('^[');
  const escOnly = (s: string) => String(s).replace(/\u001b\[[0-9;?]*[ -/]*[@-~]/g, '');
  const runningLines = caret.split('\n').filter((l) => /Running/.test(l) && /target[\\/]/.test(l));
  assert.equal(runningLines.length, 1, 'the fixture must carry exactly one Running line for this to be about it');
  assert.match(escOnly(runningLines[0]), /\^\[/, 'the byte-only stripper leaves the caret sequence intact');
  assert.doesNotMatch(ml.stripAnsi(runningLines[0]), /\^\[/);
  assert.ok(ml.stripAnsi(runningLines[0]).includes('     Running unittests src/lib.rs ('),
    'and with the caret arm the target line is readable again');
});

test('a bare `^[` in test output is not eaten — only a whole CSI sequence is', () => {
  assert.equal(ml.stripAnsi('left: "^[" right: "\\u{1b}"'), 'left: "^[" right: "\\u{1b}"');
  assert.equal(ml.stripAnsi('^[[2mdimmed^[[0m'), 'dimmed');
});

// ---------------------------------------------------------------------------
// Reading a `node --test` log
// ---------------------------------------------------------------------------

test('the frontend suite’s figures come off its TAP trailer, names off its `not ok` lines', () => {
  const s = ml.selectSuite(suitesOf('node-red'), { target: 'node' }) as Suite & { tests: number; skipped: number };
  assert.equal(s.passed, 2);
  assert.equal(s.failed, 2);
  assert.equal(s.total, 4);
  assert.deepEqual(s.failures.map((f) => f.name), [
    'a ledger row must carry the head its run measured',
    'a bullet count must equal the run’s own failed figure',
  ]);
});

test('`# pass` and `# tests` are read as the different numbers they are', () => {
  // `# pass` excludes skips, `# tests` includes them, so neither is derivable from the other
  // — and a green run here reports 2810 pass against 2818 tests.
  const green = suitesOf('green-multitarget').find((s) => s.target === 'node' && s.job === 'build (ubuntu-22.04)') as Suite & { tests: number; skipped: number };
  assert.equal(green.passed, 2810);
  assert.equal(green.tests, 2818);
  assert.equal(green.skipped, 8);
  assert.notEqual(green.passed, green.tests, 'if these were equal the pin would not discriminate');
  assert.equal(green.total, 2810, 'total stays passed+failed so it means the same thing as the cargo one');
});

// ---------------------------------------------------------------------------
// Generating
// ---------------------------------------------------------------------------

const SPEC = {
  target: 'loomux_engine',
  base: { run: '33932644347', headSha: 'a3e54a5477ab942ff44a4db883a267618dfa0e08' },
  job: 'build (ubuntu-22.04)',
  rows: [
    { n: 1, behaviour: 'session-id compare is canonicalized', run: RUN_ROUND1, cutFrom: '7b36ae86', headSha: 'ce859688cb976b93b292fa5ee16ce2a518e56a4d' },
    { n: 13, behaviour: 'a turn is opened lazily and ANNOUNCED', run: RUN_ROUND13, cutFrom: '7b36ae86', headSha: 'f9afc9655188a6882985d389311a854c6304d8a8' },
  ],
};
const LOGS = { '33932644347': log('green-multitarget'), [RUN_ROUND1]: log('red-round1-esc'), [RUN_ROUND13]: log('red-round13') };

test('the generated rows carry the figures the runs printed', () => {
  const ledger = ml.buildLedger(SPEC, LOGS);
  assert.equal(ledger.base.suite.passed, 594);
  assert.deepEqual(ledger.rows.map((r: any) => [r.n, r.passed, r.failed]), [[1, 593, 1], [13, 588, 6]]);
  assert.deepEqual(ledger.notes, [], 'both rounds reconcile to the banked green’s 594, so there is nothing to flag');
});

test('a round whose total does not reconcile with the banked green is flagged, not printed silently', () => {
  // The `ci-validate` rule this mechanizes: a round that does not reconcile is not
  // attributable to the behaviour it was cut for until someone can say why.
  const green = log('green-multitarget').split('594 passed').join('600 passed');
  const ledger = ml.buildLedger(SPEC, Object.assign({}, LOGS, { '33932644347': green }));
  assert.equal(ledger.base.suite.total, 600);
  assert.equal(ledger.notes.length, 2, 'both rounds now fail to reconcile');
  for (const n of ledger.notes) assert.match(n, /does not reconcile with the banked green's 600/);
});

test('a failing-name list shorter than the summary is flagged as a truncated block', () => {
  const cut = log('red-round13')
    .split('\n')
    .filter((l) => !/init_booted_result_is_the_decoder_walking_one_turn/.test(l))
    .join('\n');
  const ledger = ml.buildLedger(
    { target: 'loomux_engine', rows: [{ n: 13, behaviour: 'x', run: RUN_ROUND13 }] },
    { [RUN_ROUND13]: cut },
  );
  assert.equal(ledger.rows[0].failed, 6, 'the COUNT still comes off the summary line, never from counting names');
  assert.equal(ledger.rows[0].names.length, 5);
  assert.match(ledger.notes[0], /names 5 failing test\(s\) but its own summary says 6/);
});

test('the rendered table and bullets carry the run, the head and the split', () => {
  const out = String(ml.renderLedger(ml.buildLedger(SPEC, LOGS)));
  assert.match(out, /\| 1 \| session-id compare is canonicalized \| `ce859688` \| `7b36ae86` \| 33928049423 \|/);
  assert.match(out, /\*\*Round 13 — 6 tests\*\* \(588\/6, run 33928069681 @ `f9afc965`\)/);
  assert.match(out, /\*\*Round 1 — 1 test\*\* \(593\/1, run 33928049423 @ `ce859688`\)/);
  assert.match(out, /banked green[^\n]*run 33932644347 @ `a3e54a54`: 594 passed \/ 0 failed/);
});

test('a round that reddened NOTHING renders as a finding, not as a blank cell', () => {
  // `ci-validate`: publish the zero-red row rather than dropping it. A blank cell is what a
  // dropped row looks like, so the renderer says what it is instead.
  const ledger = ml.buildLedger(
    { target: 'loomux_engine', job: 'build (ubuntu-22.04)', rows: [{ n: 6, behaviour: 'a bad mutation', run: '33932644347' }] },
    { '33932644347': log('green-multitarget') },
  );
  assert.equal(ledger.rows[0].failed, 0);
  const out = String(ml.renderLedger(ledger));
  assert.match(out, /a round that reddens NOTHING is a finding/);
  assert.match(out, /\*\*Round 6 — 0 tests\*\*[^\n]*nothing reddened\./);
});

test('a failure line is neutralized for the table cell it goes into', () => {
  assert.equal(ml.abridge('assertion `left == right` failed'), "assertion 'left == right' failed");
  assert.equal(ml.abridge('assertion failed: !argv.iter().any(|a| a == "--bare")'), 'assertion failed: !argv.iter().any(\\|a\\| a == "--bare")');
  assert.equal(ml.abridge('one\ntwo   three'), 'one two three');
  assert.equal(String(ml.abridge('x'.repeat(400))).length, ml.ABRIDGE);
});

test('an abridged cell survives the round trip back through the row splitter', () => {
  // The escape only earns its place if the checker can read the cell it wrote: a `\|` that
  // `splitRow` did not honour would split one cell into two and shift every column right.
  const cell = ml.abridge('assertion failed: !argv.iter().any(|a| a == "--bare")');
  const row = `| 18 | the launch line never carries --bare | \`156c3382\` | \`7b36ae86\` | 33928080740 | \`x\` | \`${cell}\` |`;
  const cells = ml.splitRow(row) as string[];
  assert.equal(cells.length, 7);
  assert.equal(cells[4], '33928080740');
  assert.match(cells[6], /any\(\|a\| a == "--bare"\)/);
});

// ---------------------------------------------------------------------------
// Checking a posted body
// ---------------------------------------------------------------------------

const HEADS = {
  [RUN_ROUND1]: 'ce859688cb976b93b292fa5ee16ce2a518e56a4d',
  [RUN_ROUND13]: 'f9afc9655188a6882985d389311a854c6304d8a8',
};
const CHECK_LOGS = { [RUN_ROUND1]: log('red-round1-esc'), [RUN_ROUND13]: log('red-round13') };
const check = (name: string): CheckResult => ml.checkBody(body(name), CHECK_LOGS, { runHeads: HEADS }) as CheckResult;

const good = check('good');
const stale = check('stale');

test('THE NEGATIVE CONTROL: a correct body produces no MISMATCH at all', () => {
  // Without this every assertion below is satisfied by a checker that reports everything.
  assert.equal(good.counts.MISMATCH, 0, JSON.stringify(good.findings.filter((f) => f.severity === 'MISMATCH'), null, 1));
  assert.equal(good.counts.CHECK, 0);
  assert.equal(good.rows, 2, 'both rows must have been re-read — "no findings" over an empty row set is not a clean body');
  assert.ok(good.counts.OK >= 6, 'and each row must have produced its own OK rather than being skipped');
});

test('a stale head SHA is a MISMATCH naming both values', () => {
  const f = stale.findings.filter((x) => x.severity === 'MISMATCH' && /head/.test(x.message));
  assert.equal(f.length, 1);
  assert.match(f[0].message, /the row says head `deadbeef`, the run reports `ce859688`/);
});

test('a reddened set that disagrees with the log is a MISMATCH naming which side each name is on', () => {
  const f = stale.findings.filter((x) => x.severity === 'MISMATCH' && /reddened set disagrees/.test(x.message));
  assert.equal(f.length, 1);
  assert.match(String(f[0].detail), /log only: init_booted_result_is_the_decoder_walking_one_turn/);
  assert.match(String(f[0].detail), /row only: a_test_that_no_longer_exists_at_this_head/);
});

// A row whose reddened cell is EMPTY is a different defect from one whose names are wrong
// — nobody forgot to check, somebody forgot to fill in — and the two get different messages.
// Without this pin the branch that says so is redundant with the set comparison below it,
// and mutation M11 (removing it) reddens nothing at all.
test('a row that names NO reddened test where the log names some says exactly that', () => {
  const NAME = 'a_session_id_is_compared_canonically_and_a_non_uuid_is_a_mismatch';
  const emptied = body('good').split('`' + NAME + '`').join('');
  assert.notEqual(emptied, body('good'), 'the fixture edit must have landed');
  const r = ml.checkBody(emptied, CHECK_LOGS, { runHeads: HEADS }) as CheckResult;
  const f = r.findings.filter((x) => x.severity === 'MISMATCH' && /names no reddened test/.test(x.message));
  assert.equal(f.length, 1);
  assert.match(f[0].message, /the log names 1/);
  assert.match(String(f[0].detail), /a_session_id_is_compared_canonically/);
  assert.equal(r.findings.filter((x) => /reddened set disagrees/.test(x.message)).length, 0,
    'and the vaguer message is NOT also emitted for the same row');
});

test('a bullet count spelled as a WORD is read, and a wrong one is a MISMATCH', () => {
  // A checker reading only digits passes every worded bullet silently — and every bullet in
  // #2239's ledger is worded.
  const bullets = ml.readBullets(body('good')) as any[];
  assert.deepEqual(bullets.map((b) => [b.rounds[0], b.k, b.passed, b.failed]), [[1, 1, 593, 1], [13, 6, 588, 6]]);
  const wrong = stale.findings.filter((x) => x.severity === 'MISMATCH' && /the bullet says five/.test(x.message));
  assert.equal(wrong.length, 1);
  assert.match(wrong[0].message, /run 33928069681 says 6 failed/);
});

test('a bullet split that disagrees with the run is its own MISMATCH', () => {
  const f = stale.findings.filter((x) => x.severity === 'MISMATCH' && /the bullet says 589\/5/.test(x.message));
  assert.equal(f.length, 1);
  assert.match(f[0].message, /run 33928069681 says 588\/6/);
});

test('every staling in the fixture is caught, and each has its own finding', () => {
  assert.equal(stale.counts.MISMATCH, 5, JSON.stringify(stale.findings.filter((f) => f.severity === 'MISMATCH').map((f) => f.message), null, 1));
});

test('a bullet form the parser cannot read is a CHECK, never a silent pass', () => {
  const b = ml.readBullets('- **Round 2 — umpteen tests** (593/1).') as any[];
  assert.equal(b.length, 1);
  assert.equal(b[0].kRaw, 'umpteen', 'and the word is carried so the CHECK can quote it');
  assert.equal(b[0].k, null, 'an unmapped count word must not resolve to a number');
  const r = ml.checkBody(
    `${body('good')}\n- **Round 1 — umpteen tests**.\n`,
    CHECK_LOGS,
    { runHeads: HEADS },
  ) as CheckResult;
  assert.equal(r.counts.MISMATCH, 0);
  assert.equal(r.findings.filter((f) => f.severity === 'CHECK' && /could not read the bullet's count/.test(f.message)).length, 1);
});

// The bullet reader's own blind spot, pinned rather than described: the count is ONE token,
// so a multi-word count is not read at all and produces no finding of any severity. It is
// bounded by the table rows, which are re-read whatever the prose says — a bullet count is a
// SECOND reading of a figure the row already carries. Widening this to arbitrary prose would
// be a parser for English, which is the thing that cannot be right about intent.
test('a MULTI-WORD bullet count is not read at all — the stated residual', () => {
  assert.deepEqual(ml.readBullets('- **Round 2 — a couple of tests** (593/1).'), []);
  assert.equal(ml.readBullets('- **Round 2 — two tests** (593/1).').length, 1,
    'the positive control: the same sentence with a one-token count IS read');
});

test('a bullet naming a round the table does not number is a CHECK', () => {
  const r = ml.checkBody(`${body('good')}\n- **Round 99 — two tests** (1/1).\n`, CHECK_LOGS, { runHeads: HEADS }) as CheckResult;
  assert.equal(r.findings.filter((f) => f.severity === 'CHECK' && /round 99, which no table row numbers/.test(f.message)).length, 1);
});

test('a run whose log could not be fetched is a CHECK, never an OK', () => {
  const r = ml.checkBody(body('good'), { [RUN_ROUND1]: log('red-round1-esc') }, { runHeads: HEADS }) as CheckResult;
  assert.equal(r.rows, 1, 'only the row whose log was supplied is re-read');
  assert.equal(r.findings.filter((f) => f.severity === 'CHECK' && /no log available/.test(f.message)).length, 1);
});

test('a body with no ledger table is a CHECK, not a clean report', () => {
  const r = ml.checkBody('### Summary\n\nNothing here is a ledger.\n', {}, {}) as CheckResult;
  assert.equal(r.rows, 0);
  assert.equal(r.counts.MISMATCH, 0);
  assert.equal(r.findings.filter((f) => f.severity === 'CHECK' && /no ledger table found/.test(f.message)).length, 1);
});

test('the ledger table is found by what its HEADER means, not by where it sits', () => {
  const withDecoy = `| file | lines |\n|---|---|\n| a.rs | 12 |\n\n${body('good')}`;
  const t = ml.findLedgerTable(withDecoy);
  assert.ok(t, 'the decoy table must not be mistaken for the ledger');
  assert.equal(t.header[t.runCol], 'run');
  assert.match(t.header[t.redCol], /redden/);
  assert.equal(t.rows.length, 2);
  assert.equal(ml.findLedgerTable('| file | lines |\n|---|---|\n| a.rs | 12 |\n'), null);
});

test('the rendered check report ends in a summary a worker can read the MISMATCH count off', () => {
  const out = String(ml.renderCheck(stale, { pr: 2239, quiet: true }));
  assert.match(out, /SUMMARY mutation-ledger --check #2239: 5 MISMATCH, 0 CHECK, \d+ OK/);
  assert.doesNotMatch(out, /^OK /m, '--quiet must actually suppress the OK rows');
  assert.match(String(ml.renderCheck(stale, { pr: 2239 })), /^OK /m);
});

// ---------------------------------------------------------------------------
// The CLI seam
// ---------------------------------------------------------------------------

test('the CLI parses the selectors the ambiguity errors tell a worker to use', () => {
  const o = ml.parseArgs(['--rows', 'r.json', '--target', 'loomux_engine', '--job', 'build (ubuntu-22.04)', '--what', 'src/lib.rs', '--quiet']);
  assert.equal(o.rows, 'r.json');
  assert.equal(o.target, 'loomux_engine');
  assert.equal(o.job, 'build (ubuntu-22.04)');
  assert.equal(o.what, 'src/lib.rs');
  assert.equal(o.quiet, true);
  assert.throws(() => ml.parseArgs(['--nope']), /unknown argument: --nope/);
});

test('the usage text names the seam with pr-body-check rather than leaving a worker to guess', () => {
  assert.match(String(ml.USAGE), /pr-body-check` re-measures a body against HEAD and never opens a log/);
});

// `test/prbodycheck.test.ts` — the receipt checker's own pins (#2168 S1).
//
// WHAT THIS SUITE IS FOR. `scripts/pr-body-check.cjs` re-measures the figures in a posted
// PR body against the head that body claims to describe. Its whole value is that it goes
// red on the twelve figures the #2168 classification found blocking a review round, and
// stays silent on the ten merged bodies those rounds ended in. So the suite is built as a
// pair, and BOTH halves are load-bearing:
//
//   `body-good.md`   — every receipt correct. This is the NEGATIVE CONTROL. A checker that
//                      refuses everything passes every positive assertion below and fails
//                      here, which is the only thing that makes the positives mean
//                      anything.
//   `body-stale.md`  — the same body with each figure left at the value it had one round
//                      ago (the collateral-of-a-fix class, 7 of the 23 fail rounds).
//   `body-misc.md`   — the non-numeric half: an identifier that names nothing, a
//                      placeholder nobody filled, a unit stated as chars for a byte
//                      figure, a run bound to the wrong SHA, a cite past end of file.
//
// `facts.json` is everything `git` and `gh` would have said. Nothing here runs either, so
// the suite is offline and deterministic — and the fixture's numbers are all DISTINCT
// (#1182): the four instruments of one file are four different values, which is what makes
// the instrument-naming assertions fail-able rather than decorative.
//
// Every assertion that a body produces NO finding of some kind carries a positive control
// beside it, because "no finding" is also what a checker that never ran produces.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { execFileSync } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const require_ = createRequire(import.meta.url);
const here = path.dirname(fileURLToPath(import.meta.url));
const root = path.resolve(here, '..');
const scriptPath = path.join(root, 'scripts', 'pr-body-check.cjs');
const fixtures = path.join(here, 'fixtures', 'prbodycheck');

const pbc = require_(scriptPath) as any;

const FACTS = JSON.parse(fs.readFileSync(path.join(fixtures, 'facts.json'), 'utf8'));
const bodyOf = (name: string) => fs.readFileSync(path.join(fixtures, `body-${name}.md`), 'utf8');

type Finding = { severity: string; check: string; line: number; message: string };
type Result = { findings: Finding[]; counts: { MISMATCH: number; CHECK: number }; claims: Record<string, number> };

const run = (name: string): Result => pbc.analyze(bodyOf(name), FACTS) as Result;

const good = run('good');
const stale = run('stale');
const misc = run('misc');

const of = (r: Result, check: string, severity?: string) =>
  r.findings.filter((f) => f.check === check && (severity === undefined || f.severity === severity));

// ---------------------------------------------------------------------------
// The negative control, first: without it every assertion below is satisfied by a
// checker that reports everything.
// ---------------------------------------------------------------------------

test('a body whose every receipt is correct produces no MISMATCH', () => {
  assert.equal(good.counts.MISMATCH, 0, good.findings.map((f) => `${f.severity} ${f.check} L${f.line} ${f.message}`).join('\n'));
  // Positive control on the scan itself: the mechanism read this body rather than skipping
  // it. Loose floors, not second pins on the fixture's shape.
  assert.ok(good.claims.shas >= 4, `SHAs read: ${good.claims.shas}`);
  assert.ok(good.claims.diffstats >= 1, `diffstats read: ${good.claims.diffstats}`);
  assert.ok(good.claims.byte_figures >= 2, `byte figures read: ${good.claims.byte_figures}`);
});

test('the good body still reports the one thing that is genuinely unsettled', () => {
  // `65ecd6ae` is main's head during review — resolvable, correct, and not on the PR ref.
  // That is a CHECK by design, and asserting it here stops the negative control above from
  // being satisfied by a checker that has simply stopped looking at SHAs.
  const sha = of(good, 'sha', 'CHECK');
  assert.equal(sha.length, 1);
  assert.match(sha[0].message, /65ecd6ae/);
  assert.match(sha[0].message, /not on refs\/pull\/900\/head/);
});

// ---------------------------------------------------------------------------
// The stale body — the collateral-of-a-fix class, figure by figure.
// ---------------------------------------------------------------------------

test('a diffstat left at the previous round value is a MISMATCH naming both figures (#2139 r2)', () => {
  const d = of(stale, 'diffstat', 'MISMATCH');
  assert.equal(d.length, 1);
  assert.match(d[0].message, /2,673\+/);      // what the body says
  assert.match(d[0].message, /773\+/);        // what head says
  assert.equal(of(good, 'diffstat').length, 0);
});

test('a byte count stated for a named blob is checked against THAT blob (#2140 r3)', () => {
  const b = stale.findings.filter((f) => f.check === 'byte-figure' && /61855f9c/.test(f.message));
  assert.equal(b.length, 1);
  assert.equal(b[0].severity, 'MISMATCH');
  assert.match(b[0].message, /324,776 bytes/);
  assert.match(b[0].message, /325,375 bytes/);
  // The same sentence in `body-good.md` states 325,375 against the same blob and is silent,
  // so this pin fails on the value rather than on the shape of the sentence.
  assert.equal(good.findings.filter((f) => f.check === 'byte-figure').length, 0);
});

test('a byte figure with no blob beside it is checked against head, with all four instruments named (#1764 r7)', () => {
  const b = stale.findings.filter((f) => f.check === 'byte-figure' && /lessons\.md/.test(f.message));
  assert.equal(b.length, 1);
  assert.equal(b[0].severity, 'MISMATCH');
  assert.match(b[0].message, /3,361 bytes/);
  // The instrument table is the point: blob bytes, on-disk bytes, chars and lines are four
  // DIFFERENT numbers in the fixture, and all four are printed.
  assert.match(b[0].message, /blob 3,296 bytes/);
  assert.match(b[0].message, /on-disk 3,370 bytes/);
  assert.match(b[0].message, /3,281 chars/);
  assert.match(b[0].message, /74 lines/);
});

test('a line cite past the end of the file at head is a MISMATCH (#1764 r5)', () => {
  const c = of(stale, 'line-cite', 'MISMATCH');
  assert.equal(c.length, 1);
  assert.match(c[0].message, /CLAUDE\.md:782/);
  assert.match(c[0].message, /722 lines/);
});

test('a run id that does not exist, and a run whose bound SHA is not what it ran at', () => {
  const r = of(stale, 'run', 'MISMATCH');
  assert.equal(r.length, 2);
  assert.ok(r.some((x) => /33999999999 does not exist/.test(x.message)));
  assert.ok(r.some((x) => /33791843349 ran at c7a3626a/.test(x.message) && /`7396a0bd`/.test(x.message)));
  // The same run id in `body-good.md` is bound to the SHA it really ran at and is silent.
  assert.equal(of(good, 'run').length, 0);
});

test('an unresolvable SHA is a MISMATCH under both roles its sentence assigns it', () => {
  const s = of(stale, 'sha', 'MISMATCH').filter((f) => /deadbee1/.test(f.message));
  assert.equal(s.length, 2);
  assert.ok(s.some((x) => /cited as base/.test(x.message)));
  assert.ok(s.some((x) => /cited as dated-head/.test(x.message)));
});

test('a section dated to a superseded head is a CHECK, not a refusal', () => {
  // CLAUDE.md MANDATES dating a section to the head it was measured at, so a stale head is
  // a sentence to re-read, not a defect — as long as the commit is still on the PR ref.
  const s = of(stale, 'sha', 'CHECK').filter((f) => /7396a0bd/.test(f.message));
  assert.equal(s.length, 1);
  assert.match(s[0].message, /head is now c7a3626a/);
  assert.match(s[0].message, /re-read every figure this section carries/);
});

// ---------------------------------------------------------------------------
// The non-numeric half.
// ---------------------------------------------------------------------------

test('a placeholder left in the body is a MISMATCH — both the bare marker and the fill-me comment (#1758 r1)', () => {
  const p = of(misc, 'placeholder', 'MISMATCH');
  assert.equal(p.length, 2);
  assert.ok(p.some((x) => /"TODO"/.test(x.message)));
  assert.ok(p.some((x) => /RED-EVIDENCE/.test(x.message)));
  // `<!-- agent-layer -->` is the one comment a body is allowed, and `body-good.md` carries
  // it: without this the assertion above would pass on a checker that flags every comment.
  assert.equal(of(good, 'placeholder').length, 0);
});

test('a backticked identifier that names nothing at head is reported (#1751 r7)', () => {
  const i = of(misc, 'identifier');
  assert.equal(i.length, 1);
  assert.match(i[0].message, /NAME_SITES/);
  assert.equal(i[0].severity, 'CHECK');    // an external name is a legitimate reading
  // `note_cap_starvation` and `cap_starved_since_ms` in `body-good.md` both have hits, so
  // the check is not simply reporting every backticked token.
  assert.equal(of(good, 'identifier').length, 0);
  assert.ok(good.claims.identifiers >= 2, `identifiers read from the good body: ${good.claims.identifiers}`);
});

test('a figure right for one instrument and stated as another names the instrument it matched (#1764 r9)', () => {
  const b = misc.findings.filter((f) => f.check === 'byte-figure');
  assert.equal(b.length, 1);
  assert.equal(b[0].severity, 'CHECK');
  assert.match(b[0].message, /3,296 chars/);
  assert.match(b[0].message, /right for a DIFFERENT instrument \(blob bytes\)/);
});

test('a line cite inside the file prints the line it points at, at head (#1764 r3)', () => {
  const c = of(misc, 'line-cite', 'CHECK');
  assert.equal(c.length, 1);
  assert.match(c[0].message, /CLAUDE\.md:722 at head reads/);
  assert.match(c[0].message, /the guard's own doc quotes the line it was written for/);
});

// ---------------------------------------------------------------------------
// Extraction and classification, where the corpus run forced a rule.
// ---------------------------------------------------------------------------

test('a head phrase qualified by a round is a dated head, not a claim about the current one', () => {
  assert.equal(pbc.classifySha('*Everything in this fold is measured at `'), 'head-measured');
  assert.equal(pbc.classifySha('Rows M1-M3 were measured at round-1 head `'), 'dated-head');
  assert.equal(pbc.classifySha('**Review round 1 (rev-std, head `'), 'dated-head');
  assert.equal(pbc.classifySha('the branch was cut from `'), 'base');
  assert.equal(pbc.classifySha('run 33791843349 at `'), 'run-receipt');
  // The residual: an unanchored mention is classified as nothing rather than guessed at.
  assert.equal(pbc.classifySha('the head of this PR moved twice and then `'), 'unclassified');
});

test('a bare number is a run id only where its own sentence says run', () => {
  // A job id is the same shape and `gh run view` cannot resolve one; admitting it reported
  // five known-good bodies as citing runs that do not exist.
  const withRun = pbc.extract('CI run 33791843349 was green.');
  assert.deepEqual(withRun.runs.map((r: any) => r.id), ['33791843349']);
  const withJob = pbc.extract('ubuntu job `99217210184`:');
  assert.deepEqual(withJob.runs, []);
  const url = pbc.extract('see https://github.com/o/r/actions/runs/33645737625 for it');
  assert.deepEqual(url.runs.map((r: any) => r.id), ['33645737625']);
});

test('the figure checks read prose only, and the SHA checks read fenced receipts too', () => {
  const body = [
    'The head is `c7a3626a`.',
    '',
    '```',
    'panicked at src-tauri/tests/reviewdrive.rs:6495:5:',
    '4 files changed, 99 insertions(+)',
    'orphan `deadbee1`',
    '```',
  ].join('\n');
  const c = pbc.extract(body);
  assert.equal(c.diffstats.length, 0, 'a diffstat inside a fence is quoted output, not a claim');
  assert.equal(c.lineCites.length, 1, 'a line cite inside a fence is still a cite');
  assert.ok(c.shas.some((s: any) => s.sha === 'deadbee1' && s.fence), 'a SHA inside a fence is still checked');
});

test('a token is read as a path only when it carries a real file extension', () => {
  // `Buffer.length` and `assert_eq` are not paths; asking git for them printed a `fatal:`
  // for every body in the corpus.
  assert.deepEqual(pbc.pathsOn('`Buffer.length` is not a file'), []);
  assert.deepEqual(pbc.pathsOn('`src-tauri/tests/reviewdrive.rs` is'), ['src-tauri/tests/reviewdrive.rs']);
  assert.deepEqual(pbc.pathsOn('`CLAUDE.md` is'), ['CLAUDE.md']);
});

test('an identifier token needs a separator — a prose noun in backticks is not one', () => {
  for (const yes of ['note_cap_starvation', 'obs::root_action', 'is_parked()', 'NAME_SITES']) {
    assert.equal(pbc.isIdentifierToken(yes), true, yes);
  }
  for (const no of ['main', 'HEAD', 'blob', 'c7a3626a', 'src/pty.ts', '--json', '773']) {
    assert.equal(pbc.isIdentifierToken(no), false, no);
  }
});

test('one quantity stated twice with two values is grouped by its unit PHRASE, not one word', () => {
  const two = pbc.groupQuantities(pbc.tagLines('There are 17 surviving sites.\nOnly 14 surviving sites remain.'));
  assert.equal(two.length, 1);
  assert.equal(two[0].key, 'surviving sites');
  assert.deepEqual(two[0].values, [17, 14]);
  // A single word is too coarse: `3 rounds` and `7 rounds` are routinely different scopes,
  // and grouping them fires on every body in the corpus.
  assert.deepEqual(pbc.groupQuantities(pbc.tagLines('It took 3 rounds.\nThe next took 7 rounds.')), []);
});

// ---------------------------------------------------------------------------
// (h) --list-claims
// ---------------------------------------------------------------------------

test('--list-claims reads the ADDED prose of the diff, and only prose', () => {
  const added = pbc.addedProseFromDiff(fs.readFileSync(path.join(fixtures, 'diff.txt'), 'utf8'));
  const files = new Set(added.map((a: any) => a.file));
  assert.ok(files.has('doc/design/review-driver.md'));
  assert.ok(files.has('crates/loomux-engine/src/reviewdrive.rs'), 'a comment line in added code is prose');
  // The added `pub fn` and the added assignment are code, not prose, and are not read.
  assert.equal(added.filter((a: any) => /pub fn|cap_starved_since_ms = None/.test(a.text)).length, 0);
  assert.equal(added.filter((a: any) => /unchanged context line/.test(a.text)).length, 0, 'context lines are not additions');

  const rows = pbc.listClaims(added);
  const kinds = rows.map((r: any) => r.kind);
  assert.ok(kinds.includes('ordinal'), 'the ordinal this diff just made stale (#2140 B1)');
  assert.ok(kinds.includes('count'), 'a count of sites the diff itself grew');
  assert.ok(kinds.includes('absolute'), 'an only/no-other absolute');
  assert.ok(rows.some((r: any) => r.match.toLowerCase() === 'the fourth'));
  assert.equal(pbc.listClaims([]).length, 0);
});

// ---------------------------------------------------------------------------
// The CLI contract.
// ---------------------------------------------------------------------------

function cli(args: string[]): { out: string; status: number } {
  const r = execFileSync(process.execPath, [scriptPath, ...args], { encoding: 'utf8', maxBuffer: 32 * 1024 * 1024 });
  return { out: r, status: 0 };
}

test('the CLI reports through --body-file/--facts without touching git or gh, and exits 0 on a MISMATCH', () => {
  const r = cli(['--body-file', path.join(fixtures, 'body-stale.md'), '--facts', path.join(fixtures, 'facts.json')]);
  assert.equal(r.status, 0, 'exit 0 always — this is a report, never a gate');
  assert.match(r.out, /SUMMARY pr-body-check #900 @ c7a3626a: 8 MISMATCH, \d+ CHECK/);
  assert.match(r.out, /^MISMATCH diffstat/m);
});

test('--json emits the findings a caller can act on', () => {
  const r = cli(['--body-file', path.join(fixtures, 'body-good.md'), '--facts', path.join(fixtures, 'facts.json'), '--json']);
  const j = JSON.parse(r.out);
  assert.equal(j.pr, 900);
  assert.equal(j.counts.MISMATCH, 0);
  assert.ok(Array.isArray(j.findings));
});

test('--list-claims runs from a diff file with no network', () => {
  const r = cli(['--list-claims', '--diff-file', path.join(fixtures, 'diff.txt'), '--json']);
  const rows = JSON.parse(r.out);
  assert.ok(rows.length > 0);
  assert.ok(rows.every((x: any) => typeof x.file === 'string' && typeof x.kind === 'string'));
});

test('an unknown argument is reported on stderr and still exits 0', () => {
  // A crash in the checker must never read as a body defect.
  const r = execFileSync(process.execPath, [scriptPath, '--nope'], { encoding: 'utf8' });
  assert.equal(r, '');
});

// ---------------------------------------------------------------------------
// A fact that is ABSENT is never a MISMATCH: a fixture may cover one axis without
// silently blessing every other.
// ---------------------------------------------------------------------------

test('with no facts at all, nothing is a MISMATCH and the diffstat is reported as unchecked', () => {
  const r = pbc.analyze(bodyOf('stale'), { pr: 900 }) as Result;
  assert.equal(r.findings.filter((f) => f.severity === 'MISMATCH' && f.check === 'diffstat').length, 0);
  assert.equal(of(r, 'diffstat', 'CHECK').length, 1);
  assert.match(of(r, 'diffstat', 'CHECK')[0].message, /no diffstat was measured/);
});

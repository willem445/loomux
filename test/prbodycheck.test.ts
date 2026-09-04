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

test('a blob cited beside a file that is not that file blob at head is a CHECK (#2139 r2)', () => {
  // The append proof's BASE blob. It is correct and the script cannot read that it is
  // correct — a blob cited for a file is as often last round's as this round's — so it
  // asks rather than refuses, and names both blobs so the answer is one glance away.
  const b = of(good, 'blob');
  assert.equal(b.length, 1);
  assert.equal(b[0].severity, 'CHECK');
  assert.match(b[0].message, /blob `728f7407` is cited beside src-tauri\/tests\/reviewdrive\.rs/);
  assert.match(b[0].message, /whose blob at head is 61855f9c/);
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
  const b = stale.findings.filter((f) => f.check === 'byte-figure' && /324,776/.test(f.message));
  assert.equal(b.length, 1);
  assert.equal(b[0].severity, 'MISMATCH');
  // The message shape is asserted, not just the numbers: only the blob-pairing arm says
  // "stated for blob ... (git cat-file -s)". Matching on the values alone passes just as
  // well when the figure has fallen through to the compare-against-head arm, which is a
  // different check answering a different question.
  assert.match(b[0].message, /is stated for blob `61855f9c`/);
  assert.match(b[0].message, /matches no blob bound to it on this line \(git cat-file -s\)/);
  assert.match(b[0].message, /`61855f9c` = 325,375 bytes \/ 323,044 chars \/ 6,892 lines/);

  // The discriminating half. `body-good.md` states the BASE blob's size beside the head
  // blob's, with the path on the same line: pairing each figure with the blob next to it
  // settles both and says nothing. Without pairing, the base figure is measured against
  // the file at head and 300,527 becomes a finding — so this assertion is what makes the
  // pairing rule fail-able rather than merely exercised.
  assert.equal(good.findings.filter((f) => f.check === 'byte-figure').length, 0);
  assert.ok(good.claims.byte_figures >= 2, `byte figures read from the good body: ${good.claims.byte_figures}`);
});

test('two blob tokens straddling one figure are a CHECK, not a refusal (rev-std round 1, premortem 2)', () => {
  // The reviewer's arrangement: "blob X ... figure ... blob Y", where nearest-to-the-left
  // pairs the figure with the blob the sentence is contrasting it against. No rule can read
  // which one the prose means, so the script says exactly that.
  const straddled = 'The blob `728f7407` grew to **325,375** bytes at `61855f9c`.';
  const s = (pbc.analyze(straddled, FACTS) as Result).findings.filter((f) => f.check === 'byte-figure');
  assert.equal(s.length, 1);
  assert.equal(s[0].severity, 'CHECK');
  assert.match(s[0].message, /nearest blob `728f7407` but matches `61855f9c`/);
  assert.match(s[0].message, /two blobs straddle this figure/);

  // The discriminating half, and the reason "matches ANY blob on the line" is NOT the rule:
  // a figure matching neither candidate is still a MISMATCH. On the real #2140 body the
  // stale figure sits in the same sentence as the wave-1 blob it came from, so a
  // membership rule reads that defect as clean.
  const wrong = 'The blob `728f7407` grew to **324,776** bytes at `61855f9c`.';
  const w = (pbc.analyze(wrong, FACTS) as Result).findings.filter((f) => f.check === 'byte-figure');
  assert.equal(w.length, 1);
  assert.equal(w[0].severity, 'MISMATCH');
  assert.match(w[0].message, /`728f7407` = 300,527 bytes/);
  assert.match(w[0].message, /`61855f9c` = 325,375 bytes/);
});

test('a blob further than the binding window is not that figure subject', () => {
  // The window and the boundary are two separate constraints, and the arrow test below
  // exercises only the boundary. Here `728f7407` sits well past BLOB_BIND_CHARS with no
  // punctuation between it and the figure, so only the window can exclude it: bound, it
  // would be the nearest candidate, would not match, and no other candidate is on the line
  // — a MISMATCH on a sentence that claims nothing about it.
  const far = 'Blob `728f7407` was the subject of a much earlier round and is mentioned here only in passing before we get to **325,375** bytes';
  assert.ok(far.indexOf('325,375') - far.indexOf('728f7407') > 40, 'the fixture must actually exceed the window');
  assert.doesNotMatch(far.slice(far.indexOf('728f7407'), far.indexOf('325,375')), /[.;|—→]/, 'and must carry no boundary, or the window is not what is under test');
  assert.equal((pbc.analyze(far, FACTS) as Result).findings.filter((f) => f.check === 'byte-figure').length, 0);

  // Positive control: the identical sentence with the blob inside the window IS bound, and
  // the same figure then reddens — so the silence above is the window, not the checker
  // having stopped looking at this line.
  const near = 'Blob `728f7407` grew to **325,375** bytes';
  const n = (pbc.analyze(near, FACTS) as Result).findings.filter((f) => f.check === 'byte-figure');
  assert.equal(n.length, 1);
  assert.equal(n[0].severity, 'MISMATCH');
});

test('binding is indentation-invariant at BOTH sites (rev-final round 3, W1c)', () => {
  // The defect: the boundary was scanned in `text` (trimmed) using indices taken against
  // the untrimmed line, so the scanned span was shifted right by the indent — it dropped
  // the leading W characters of the true span and read W characters past its end. Once W
  // exceeded the token's own length the DROPPED prefix held the boundary, and a blob the
  // rule excludes became the nearest candidate: a false MISMATCH, the refusing direction,
  // on a tool whose contract is that MISMATCH means a real defect.
  //
  // The corpus could not have caught it — no body in it carries a prose line indented that
  // far — so the pin is this loop over the axis, not a single probe. It also retires the
  // reviewer's premortem 2: nothing here depends on a threshold, so a body that later
  // indents a receipt under a nested bullet cannot start reporting.
  // The figure must NOT match the blob it would wrongly bind to, or binding cannot change
  // the verdict and the loop is silent under every implementation — a fixture that cannot
  // discriminate (#1182), which is what the mutation round caught in the first draft of
  // this test. 325,375 is `61855f9c`'s size, not `728f7407`'s.
  const FIG = 'blob `728f7407`. figure 325,375 bytes';
  const RUN = 'run 33791843349. Separately `7396a0bd` is the round-1 head';
  for (const indent of [0, 1, 2, 4, 8, 9, 10, 12, 14, 20, 30, 41]) {
    const pad = ' '.repeat(indent);
    const fig = (pbc.analyze(pad + FIG, FACTS) as Result).findings.filter((f) => f.check === 'byte-figure');
    assert.deepEqual(fig, [], `byte figure at indent ${indent}: the "." ends the binding at every indent`);
    const run = (pbc.analyze(pad + RUN, FACTS) as Result).findings.filter((f) => f.check === 'run');
    assert.deepEqual(run, [], `run/SHA at indent ${indent}: the "." ends the binding at every indent`);
  }

  // Positive controls, so the twelve silences above are the boundary rule and not a
  // checker that has stopped reading indented lines at all. Same sentences, boundary
  // removed, at an indent the defect used to fire at.
  const boundFig = (pbc.analyze(`${' '.repeat(14)}blob \`728f7407\` at 300,527 bytes`, FACTS) as Result).findings.filter((f) => f.check === 'byte-figure');
  assert.equal(boundFig.length, 0, 'bound and matching -> silent');
  const badFig = (pbc.analyze(`${' '.repeat(14)}blob \`728f7407\` at 300,528 bytes`, FACTS) as Result).findings.filter((f) => f.check === 'byte-figure');
  assert.equal(badFig.length, 1);
  assert.equal(badFig[0].severity, 'MISMATCH');
  const badRun = (pbc.analyze(`${' '.repeat(14)}run 33791843349 at \`7396a0bd\``, FACTS) as Result).findings.filter((f) => f.check === 'run');
  assert.equal(badRun.length, 1);
  assert.equal(badRun[0].severity, 'MISMATCH');
});

test('arm 3 is silent because there is no subject to measure (rev-final round 3, W1b)', () => {
  // The comment above `analyze`'s byte-figure block promises what happens here, and an
  // earlier draft promised an instrument table and a CHECK. It does not do that, and
  // should not: a figure naming no measured file has no subject to tabulate, and a line
  // naming several is the per-file table shape that the `per-file` check reads instead.
  // Pinned so the correction cannot go false quietly (the documented-escape-hatch rule).
  const none = pbc.analyze('One 135-char prose line was re-wrapped.', SMALL) as Result;
  assert.deepEqual(of(none, 'byte-figure'), [], 'zero measured paths named -> silent');

  // "Measured" means present in `facts.files`, not merely named in the numstat — so the
  // two paths here are two the fixture really measures.
  const several = pbc.analyze('`CLAUDE.md` and `.orrerix/lessons.md` are 4,211 bytes between them.', FACTS) as Result;
  assert.deepEqual(of(several, 'byte-figure'), [], 'several measured paths named -> silent');

  // Positive control: arm 2 on the same facts DOES speak, so the two silences are arm 3's
  // fall-through rather than a check that never ran.
  const one = pbc.analyze('`doc/design/a.md` is 4,211 bytes.', SMALL) as Result;
  const o = of(one, 'byte-figure');
  assert.equal(o.length, 1);
  assert.equal(o[0].severity, 'MISMATCH');
  assert.match(o[0].message, /blob 100 bytes .* 98 chars .* 10 lines/);
});

test('an arrow separates an append proof into two claims, each bound to its own blob', () => {
  // `base X N bytes -> head Y M bytes` is two claims, and binding across the arrow makes
  // the head figure a candidate for the base blob. Both halves are correct here and both
  // are silent; swapping one value reddens only that half.
  const proof = 'base `728f7407` **300,527** bytes → head `61855f9c` **325,375** bytes';
  assert.equal((pbc.analyze(proof, FACTS) as Result).findings.filter((f) => f.check === 'byte-figure').length, 0);
  const broken = 'base `728f7407` **300,527** bytes → head `61855f9c` **324,776** bytes';
  const r = (pbc.analyze(broken, FACTS) as Result).findings.filter((f) => f.check === 'byte-figure');
  assert.equal(r.length, 1);
  assert.equal(r[0].severity, 'MISMATCH');
  assert.match(r[0].message, /"324,776 bytes" is stated for blob `61855f9c`/);
  // ...and the base blob is NOT among its candidates, because the arrow ended the binding.
  assert.doesNotMatch(r[0].message, /728f7407/);
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
// Three more Part 1 rows, each stated in the shape its own round was failed in.
// ---------------------------------------------------------------------------

const SMALL = {
  pr: 900,
  head: 'c7a3626a',
  mergeBase: '517073c4',
  diffstat: { files: 2, insertions: 17, deletions: 2 },
  numstat: { 'doc/design/a.md': { insertions: 9, deletions: 0 }, 'doc/design/b.md': { insertions: 8, deletions: 2 } },
  files: { 'doc/design/a.md': { blob: 'aabbccdd', blobBytes: 100, blobChars: 98, blobLines: 10, diskBytes: 110, lineAt: {} } },
};

test('a numstat written from recollection is caught even when its figures are in code spans (#2105 r2)', () => {
  // The worker's own words on that round: "that figure was never measured — I wrote it
  // from recollection". The real edit was 9 + 8 insertions and 2 deletions.
  const r = pbc.analyze('The change is `9 + 6` insertions, `0` deletions across the two notes.', SMALL) as Result;
  const i = of(r, 'insertions');
  assert.equal(i.length, 1);
  assert.match(i[0].message, /"6 insertions" is neither the head total \(17\)/);
  // The discriminating half: the same sentence with the figures that DO reconcile is
  // silent, so this fails on the value and not on the code spans.
  const ok = pbc.analyze('The change is `9 + 8` insertions, `2` deletions across the two notes.', SMALL) as Result;
  assert.equal(of(ok, 'insertions').length, 0);
  assert.equal(of(ok, 'deletions').length, 0);
});

test('two size figures on a line naming one file are a CHECK with the instrument table (#1764 r7)', () => {
  const r = pbc.analyze('The heading section of `doc/design/a.md` is 7 lines, and the new one is 35 lines.', SMALL) as Result;
  const b = of(r, 'byte-figure', 'CHECK');
  assert.equal(b.length, 2);
  assert.ok(b.every((x) => /the line states several figures/.test(x.message)));
  assert.ok(b.every((x) => /blob 100 bytes .* 98 chars .* 10 lines/.test(x.message)));
});

test('one quantity stated twice with two values is reported with both lines (#1751 r5)', () => {
  const r = pbc.analyze('There are 17 surviving sites.\n\nOnly 14 surviving sites remain after the sweep.', SMALL) as Result;
  const q = of(r, 'quantity');
  assert.equal(q.length, 1);
  assert.match(q[0].message, /"surviving sites" is stated with 2 different values: 17, 14/);
  assert.match(q[0].message, /lines 1, 3/);
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

// `test/personas-prbodycheck.test.ts` — the persona-scan pin for #2168's
// receipt-checker rollout (#2168 S3, extending the shape #1968 mandated for the
// two-layer prose rule).
//
// WHAT IT PINS. `scripts/pr-body-check.cjs` (#2168 S1) re-derives every receipt a
// posted PR body states, so the lanes that used to spend their rounds on body figures
// spend them on reasoning instead. That only works if the personas actually name the
// script and the duplicate-post rule, and prose is editable — a rename or a rewrite
// silently deletes the witness. This scan reads the live persona files and fails when
// either reviewer persona stops naming:
//
//   `pr-body-check`      — the script, run FIRST, its output reported as claims 1–n
//                          (rev-std) / re-verified from it (rev-final, rev-lead);
//   the duplicate-post   — read `gh pr view --json reviews` before posting and refuse
//   rule                   a byte-identical review body; a round is a distinct
//                          (lane, head, digest), not a post count; a kickoff round
//                          number is a floor, never a count to inflate.
//
// COORDINATION (#2168 S2/S3, never two scans). S2 (`personas/2168-worker-prereport`)
// pins the three worker personas against the same script. Whoever merges first owns
// this one file: S2 lands first → S3 rebases and extends it; S3 landed first → S2
// extends this file. Never a second persona-scan test file.
//
// Every assertion below reads the file that ships, not a fixture: a test fixture could
// pass while the shipped persona drifted, which is exactly the drift the pin exists
// for. The script-existence assertion ties the persona's pointer to a real file — a
// persona naming a script that is not there is a dangling instruction.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const here = path.dirname(fileURLToPath(import.meta.url));
const root = path.resolve(here, '..');

const personaOf = (name: string) =>
  fs.readFileSync(path.join(root, '.github', 'agents', name), 'utf8');

const revStd = personaOf('rev-std.md');
const revFinal = personaOf('rev-final.md');
const revLead = personaOf('rev-lead.md');

// Positive control for the read itself: a persona file is frontmatter + a body that
// opens with its identity. If this fails, the assertions below measured nothing.
// The line endings are CRLF on disk (this repo's Windows baseline), so the anchor is
// spelled with \r? — an LF-only anchor is the "anchor not found on a string you can
// see" signature (#1196).
test('the scan reads the real persona files, not empty or foreign ones', () => {
  assert.match(revStd, /^---\r?\n[\s\S]*?name: rev-std/);
  assert.match(revFinal, /^---\r?\n[\s\S]*?name: rev-final/);
  assert.match(revLead, /^---\r?\n[\s\S]*?name: rev-lead/);
  for (const [name, text] of [['rev-std', revStd], ['rev-final', revFinal], ['rev-lead', revLead]] as const) {
    assert.ok(text.length > 2000, `${name}.md is implausibly short — the scan read the wrong file`);
  }
});

test('the script the personas name exists on disk', () => {
  assert.ok(
    fs.existsSync(path.join(root, 'scripts', 'pr-body-check.cjs')),
    'a persona naming scripts/pr-body-check.cjs that is not there is a dangling instruction',
  );
});

test('rev-std runs pr-body-check FIRST and reports its output as the claim list', () => {
  assert.match(revStd, /pr-body-check\.cjs/, 'rev-std must name the script');
  assert.match(revStd, /FIRST/, 'the ordering is the pin: the script runs before judgment');
  assert.match(revStd, /MISMATCH/, 'the report carries the script\'s MISMATCH/CHECK contract');
});

test('rev-final names pr-body-check and re-verifies from it, not from the paste', () => {
  assert.match(revFinal, /pr-body-check\.cjs/, 'rev-final must name the script');
});

test('rev-lead names pr-body-check where it checks the body claim-by-claim', () => {
  assert.match(revLead, /pr-body-check\.cjs/, 'rev-lead must name the script');
});

for (const [name, text] of [['rev-std', revStd], ['rev-final', revFinal]] as const) {
  test(`${name} refuses to post a byte-identical review body (the dup-post rule)`, () => {
    assert.match(text, /--json reviews/, `${name} must name the pre-post check command`);
    assert.match(text, /byte-identical/, `${name} must name the refusal shape`);
    assert.match(text, /lane, head, digest/, `${name} must define a round as a distinct (lane, head, digest), not a post count`);
    assert.match(text, /floor/, `${name} must state kickoff round numbers are a floor, not a count to inflate`);
  });
}
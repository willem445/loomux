// `test/personas-prbodycheck.test.ts` — the persona-scan pin for #2168's
// receipt-checker rollout (#2168 S3, extending the shape #1968 mandated for the
// two-layer prose rule).
//
// WHAT IT PINS. `scripts/pr-body-check.cjs` (#2168 S1) re-derives every receipt a
// posted PR body states, so the lanes that used to spend their rounds on body figures
// spend them on reasoning instead. That only works if the personas actually name the
// script and (for the reviewers) the duplicate-post rule, and prose is editable — a
// rename or a rewrite silently deletes the witness. This scan reads the live persona
// files and fails when any persona stops naming its rule: the reviewer personas below,
// and (S2 extension) every worker persona against the one pre-report step.
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
// extends this file. Never a second persona-scan test file. RESOLVED the second way:
// #2234 merged first (S3's scan is main's), so S2 rebased onto main and EXTENDS THIS
// FILE — the reviewer half above is main's, the worker half below is S2's. One file,
// both halves.
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
import { parseWorkflow } from '../src/workflowmodel.ts';

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

// --- S2 extension: the worker personas (#2168 S2) ---
//
// Same discipline as the reviewer tests above (#1968's twolayer.rs shape:
// derived population, population floor, roster tie). The population is the
// `worker-*.md` name pattern in `.github/agents/` — the convention worker
// personas are filed under — NOT the live roster: `worker-quick.md` is a
// committed persona even though this group's roster does not point at it,
// and `process.md` is not a worker persona even though its block is
// worker-kind (it is S4's surface, a different slice). The roster-tie test
// below keeps the two honest: a worker-kind roster block whose profile the
// file scan did not cover is a hole this pin names.
//
// Residual, stated: a worker-kind roster block pointing at a NON-worker-named
// profile is outside both populations by construction; today that is only
// `process.md`, and the roster-tie test below fails if a second one appears.
//
// The worker step this pins (`.github/agents/worker-*.md`, #2168 S2): one
// numbered step before `report(done)` — run the script from the worktree at
// the PR head and paste its summary line into the agent layer (MISMATCH
// zero), no figure from recollection (base AND head), a property over a
// count, twins for every claim the diff edits, red-before-green on a routed
// behaviour change — repeated after EVERY push and on a body-only fix.

const workerPersonaFiles = fs
  .readdirSync(path.join(root, '.github', 'agents'))
  .filter((f) => /^worker-.+\.md$/.test(f))
  .sort();

test('the scan reads the real worker persona files, not empty or foreign ones', () => {
  assert.ok(
    workerPersonaFiles.length >= 3,
    `only ${workerPersonaFiles.length} worker persona file(s) found (${workerPersonaFiles.join(', ')}); ` +
      'a population this small means the scan found almost nothing, not that the rule stopped applying',
  );
  for (const f of workerPersonaFiles) {
    const text = personaOf(f);
    const expectedName = f.replace(/\.md$/, '');
    assert.match(
      text,
      new RegExp(`^---\\r?\\n[\\s\\S]*?name: ${expectedName}\\r?\\n`),
      `${f} is not the persona its filename names`,
    );
    assert.ok(text.length > 2000, `${f} is implausibly short — the scan read the wrong file`);
  }
});

test('every worker persona names the pre-report receipt check', () => {
  for (const f of workerPersonaFiles) {
    assert.match(
      personaOf(f),
      /pr-body-check/,
      `${f} must name the pre-report receipt check (scripts/pr-body-check.cjs, #2168 S2)`,
    );
  }
});

// The name alone is not the pin: a persona rewrite that keeps the string
// `pr-body-check` but collapses the (a)-(e) step passes the test above while
// deleting the substance. These assert the parts of the step a reviewer can
// re-derive mechanically — the invocation form, the MISMATCH-zero contract,
// and the after-every-push trigger — the same contract pins the reviewer half
// makes (FIRST / MISMATCH).
test('every worker persona keeps the receipt-check step, not just its name', () => {
  for (const f of workerPersonaFiles) {
    const text = personaOf(f);
    assert.match(text, /--pr/, `${f} must state the script's --pr invocation form`);
    assert.match(text, /MISMATCH/, `${f} must state the MISMATCH-zero contract`);
    assert.match(text, /EVERY push/, `${f} must carry the after-EVERY-push trigger`);
    assert.match(text, /--list-claims/, `${f} must name the --list-claims twin sweep`);
  }
});

test("the live roster's worker blocks point at personas the scan covered", () => {
  const workflowText = fs.readFileSync(path.join(root, '.orrerix', 'workflow.yml'), 'utf8');
  const { workflow, findings: syntax } = parseWorkflow(workflowText);
  assert.deepEqual(
    syntax.map((f) => `${f.severity} ${f.code}: ${f.message}`),
    [],
    "loomux's own workflow file must parse clean before its roster can be read",
  );
  assert.ok(workerPersonaFiles.length > 0, 'no worker persona files found; the scan below would be vacuous');
  // The roster decides what is live, the file scan decides what is checked,
  // and the two must not disagree for the worker-* namespace.
  const uncovered: string[] = [];
  for (const b of workflow.blocks) {
    if (b.kind !== 'worker' || !b.profile) continue;
    const rel = b.profile.replace(/^\.\//, '');
    if (!/^worker-.+\.md$/.test(rel)) continue; // e.g. process.md — S4's, stated residual above
    if (!workerPersonaFiles.includes(rel)) uncovered.push(`${b.id} -> ${rel}`);
  }
  assert.deepEqual(
    uncovered,
    [],
    'worker-kind roster block(s) point at worker personas the receipt-check scan did not cover: ' +
      uncovered.join(', '),
  );
});

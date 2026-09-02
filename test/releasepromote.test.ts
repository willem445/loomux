// Repo-file pin on `.github/workflows/release.yml`'s `promote` job — same
// species as `workflowdogfood.test.ts`'s pin on `.orrerix/workflow.yml`: read
// the real file straight off disk (not a fixture, which would drift the
// moment someone edits the workflow) and assert on the exact invariant a
// live incident cost us, so the next "simplify these two calls back into
// one" reintroduces a RED test, not a silent regression.
//
// #341/#543: `promote`'s "Publish the draft release" step sent
// `draft=false` and `make_latest=true` in a single `gh api -X PATCH` call
// for stable tags. That combination let GitHub silently drop `make_latest`
// (the release still counted as a draft at the instant the combined
// request was evaluated) — the call still succeeded, `promote` stayed
// green, but v1.0.0 never took the `latest` pointer from v0.10.0. There is
// no way to exercise `promote`'s real run from a PR (it only fires on a
// pushed `vX.Y.Z` tag), so this pins the textual shape of the fix instead:
// no `gh api` call in this step may ever carry both `draft=` and
// `make_latest=` together, in either direction, and the stable/beta split
// must still address the release by RELEASE_ID, never a tag-based lookup
// (#282).
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

const text = readFileSync(new URL("../.github/workflows/release.yml", import.meta.url), "utf8");

// Scoped to just the "Publish the draft release" step's `run:` block — the
// actual shell, not the step's own explanatory comments (which legitimately
// mention `gh release edit <tag>` in prose, describing what NOT to do) —
// so this pin can't accidentally pass, or fail, on comment text rather
// than the real `gh api` calls.
function publishStepRunBlock(src: string): string {
  const stepStart = src.indexOf("- name: Publish the draft release");
  assert.ok(stepStart >= 0, "the 'Publish the draft release' step must exist in promote");
  const runStart = src.indexOf("run: |", stepStart);
  assert.ok(runStart >= 0, "the step must have a `run: |` block");
  const bodyStart = src.indexOf("\n", runStart) + 1;
  const nextStepOrJob = src.slice(bodyStart).search(/\n\s{0,6}(- name:|\S+:\s*$)/m);
  const end = nextStepOrJob >= 0 ? bodyStart + nextStepOrJob : src.length;
  return src.slice(bodyStart, end);
}

function ghApiLines(step: string): string[] {
  return [...step.matchAll(/^\s*gh api .+$/gm)].map((m) => m[0]);
}

// Same scoping rule as publishStepRunBlock, for promote's "Verify asset
// count before promoting" step — the run block only, not the job's env
// comment block, so the pins below read the real shell, never prose.
function verifyStepRunBlock(src: string): string {
  const stepStart = src.indexOf("- name: Verify asset count before promoting");
  assert.ok(stepStart >= 0, "the 'Verify asset count before promoting' step must exist in promote");
  const runStart = src.indexOf("run: |", stepStart);
  assert.ok(runStart >= 0, "the step must have a `run: |` block");
  const bodyStart = src.indexOf("\n", runStart) + 1;
  const nextStepOrJob = src.slice(bodyStart).search(/\n\s{0,6}(- name:|\S+:\s*$)/m);
  const end = nextStepOrJob >= 0 ? bodyStart + nextStepOrJob : src.length;
  return src.slice(bodyStart, end);
}

test("promote's publish step never combines make_latest with the draft flip, in either direction", () => {
  const step = publishStepRunBlock(text);
  const lines = ghApiLines(step);
  assert.ok(lines.length >= 2, "expected at least 2 `gh api` calls: the draft flip and a make_latest call");

  for (const line of lines) {
    const hasDraft = /-f draft=/.test(line);
    const hasMakeLatest = /-f make_latest=/.test(line);
    assert.ok(
      !(hasDraft && hasMakeLatest),
      `a single \`gh api\` call must never set both draft= and make_latest= — this is the exact combination ` +
        `that silently dropped make_latest on v1.0.0 (#341/#543): ${line}`
    );
  }
});

test("promote's publish step sets make_latest explicitly for both tag kinds, in follow-up calls", () => {
  const step = publishStepRunBlock(text);
  // The stable branch is the inverse of `create-release`'s own
  // `tag.includes("-")` prerelease test — this must keep using the same
  // shape (`!= *-*`), not a re-derived condition that could drift from it.
  assert.match(
    step,
    /GITHUB_REF_NAME"\s*!=\s*\*-\*/,
    "must branch on the stable (non-hyphenated) tag shape, matching create-release's prerelease test"
  );
  assert.match(step, /-f make_latest=true/, "the stable branch must explicitly set make_latest=true");
  assert.match(step, /-f make_latest=false/, "the beta/RC branch must explicitly set make_latest=false");
});

test("promote's publish step stays RELEASE_ID-addressed — never a tag-based lookup (#282)", () => {
  const step = publishStepRunBlock(text);
  assert.doesNotMatch(step, /gh release edit/, "must never fall back to a tag-based `gh release edit` lookup");
  for (const line of ghApiLines(step)) {
    assert.match(line, /\$RELEASE_ID/, `every \`gh api\` call in this step must address RELEASE_ID: ${line}`);
  }
});

// The behavioral half of #1962. promote's asset-count gate is extracted to
// scripts/check-release-assets.js precisely so it can be EXECUTED without a
// real release (promote only fires on a pushed `vX.Y.Z` tag, so CI can never
// run the workflow step itself): these spawn the very file the workflow step
// calls, against N-1 / N / N+1 for BOTH expected constants.
const countScript = fileURLToPath(new URL("../scripts/check-release-assets.js", import.meta.url));

function runCountCheck(args: string[]) {
  return spawnSync(process.execPath, [countScript, ...args], { encoding: "utf8" });
}

test("the count check accepts N and refuses N-1 and N+1, for both expected constants (#1962)", () => {
  for (const expected of [10, 9]) {
    // EXPECTED_ASSETS_STABLE / EXPECTED_ASSETS_BETA — the same values the
    // workflow-pin test below asserts, so the two cannot drift apart.

    const match = runCountCheck([String(expected), String(expected)]);
    assert.equal(match.status, 0, `N (${expected}/${expected}) must promote`);
    assert.match(match.stdout, /Asset count matches/, "the accept path must say so, not exit silently");

    const under = runCountCheck([String(expected - 1), String(expected)]);
    assert.equal(under.status, 1, `N-1 (${expected - 1}/${expected}) must refuse promotion`);
    assert.match(under.stderr, /FEWER than expected/, "the deficit refusal must name the direction");
    assert.ok(
      under.stderr.includes(`${expected - 1}/${expected}`),
      `the deficit refusal must carry the actual vs expected numbers: ${under.stderr}`
    );

    const over = runCountCheck([String(expected + 1), String(expected)]);
    assert.equal(
      over.status,
      1,
      `N+1 (${expected + 1}/${expected}) must refuse — the old -lt check promoted a surplus uncounted (#1962)`
    );
    assert.match(over.stderr, /MORE than expected/, "the surplus refusal must name the direction");
    assert.match(over.stderr, /#282/, "the surplus refusal must diagnose it as the #282 duplicate-upload class");
    assert.ok(
      over.stderr.includes(`${expected + 1}/${expected}`),
      `the surplus refusal must carry the actual vs expected numbers: ${over.stderr}`
    );
  }
});

test("the count check refuses malformed counts instead of comparing garbage", () => {
  for (const args of [["ten", "10"], ["-3", "10"], ["11"]]) {
    const bad = runCountCheck(args);
    assert.equal(
      bad.status,
      2,
      `malformed args ${JSON.stringify(args)} must exit 2 (usage), not 0/1 (a count verdict)`
    );
  }
});

test("promote's count step delegates to the script and keeps the expected constants (#1962)", () => {
  const step = verifyStepRunBlock(text);
  assert.match(
    step,
    /node scripts\/check-release-assets\.js "\$count" "\$expected"/,
    "promote must call scripts/check-release-assets.js — the inline shell check is the one that let a surplus through (#1962)"
  );
  assert.doesNotMatch(
    step,
    /-lt\b|-ne\b|-gt\b/,
    "no inline shell comparison may survive in the step — the script owns the check now"
  );
  // The script call needs the repo on disk, so promote must check it out
  // (it previously ran on the bare runner). Scoped to the span between the
  // job key and the verify step, not the whole file's other checkout uses.
  const promoteStart = text.indexOf("\n  promote:");
  const verifyStart = text.indexOf("- name: Verify asset count before promoting");
  assert.ok(promoteStart >= 0 && verifyStart > promoteStart, "promote's job and verify step must exist");
  assert.match(
    text.slice(promoteStart, verifyStart),
    /uses: actions\/checkout@v4/,
    "promote must check out the repo — the count check calls scripts/check-release-assets.js"
  );
  // Not part of this change; pinned so the behavioral test's constants and
  // the workflow's cannot drift apart silently.
  assert.match(text, /^ {6}EXPECTED_ASSETS_STABLE: 10$/m, "the stable expected count is unchanged by #1962");
  assert.match(text, /^ {6}EXPECTED_ASSETS_BETA: 9$/m, "the beta expected count is unchanged by #1962");
});

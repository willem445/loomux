// Repo-file pin on `.github/workflows/release.yml`'s `promote` job — same
// species as `workflowdogfood.test.ts`'s pin on `.loomux/workflow.yml`: read
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

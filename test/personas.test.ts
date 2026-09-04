// Every worker persona names the pre-report receipt check (#2168 S2).
//
// SHAPE: `src-tauri/tests/twolayer.rs` (#1968) — the population is DERIVED,
// never listed, and a population control counted at the verified site turns a
// parse that found nothing into a red rather than a silent pass. That pin
// derives from `.orrerix/workflow.yml` through the real parser because its
// rule binds the LIVE roster; this one deliberately does not, and the
// difference is the classification, not the discipline. The S2 rule binds the
// committed worker persona FILES: `worker-quick.md` is one even though this
// group's roster does not currently point at it, and `process.md` is not one
// even though its block is worker-kind (it is S4's surface, a different
// slice). So the population is the `worker-*.md` name pattern in
// `.github/agents/` — the convention worker personas are filed under — and
// the second test ties the live roster to it: a worker-kind block whose
// profile the file scan did not cover is a hole this pin names.
//
// Residual, stated: a worker-kind roster block pointing at a NON-worker-named
// profile is outside both populations by construction; today that is only
// `process.md` (S4's), and the roster-tie test below fails if a second one
// appears.
import { test } from "node:test";
import assert from "node:assert/strict";
import { readdirSync, readFileSync } from "node:fs";
import { parseWorkflow } from "../src/workflowmodel.ts";

const agentsDir = new URL("../.github/agents/", import.meta.url);

function workerPersonaFiles(): string[] {
  return readdirSync(agentsDir)
    .filter((f) => /^worker-.+\.md$/.test(f))
    .sort();
}

test("every worker persona names the pre-report receipt check", () => {
  const files = workerPersonaFiles();
  // Loose floor, as in twolayer.rs: three is the roster as it stands
  // (worker-std, worker-deep, worker-quick), and a roster or rename that
  // changes the count must not read as a defect — but an empty or near-empty
  // population means the scan found almost nothing, not that the rule stopped
  // applying.
  assert.ok(
    files.length >= 3,
    `only ${files.length} worker persona file(s) found (${files.join(", ")}); ` +
      "a population this small means the scan found almost nothing, not that the rule stopped applying"
  );
  for (const f of files) {
    const text = readFileSync(new URL(f, agentsDir), "utf8").replace(/\r\n/g, "\n");
    assert.match(
      text,
      /pr-body-check/,
      `${f} must name the pre-report receipt check (scripts/pr-body-check.cjs, #2168 S2)`
    );
  }
});

test("the live roster's worker blocks point at personas the scan covered", () => {
  const text = readFileSync(new URL("../.orrerix/workflow.yml", import.meta.url), "utf8");
  const { workflow, findings: syntax } = parseWorkflow(text);
  assert.deepEqual(
    syntax.map((f) => `${f.severity} ${f.code}: ${f.message}`),
    [],
    "loomux's own workflow file must parse clean before its roster can be read"
  );
  const covered = workerPersonaFiles();
  assert.ok(covered.length > 0, "no worker persona files found; the scan below would be vacuous");
  // A worker-kind block whose persona the file scan did NOT cover is a hole:
  // the roster decides what is live, the file scan decides what is checked,
  // and the two must not disagree for the worker-* namespace.
  const uncovered: string[] = [];
  for (const b of workflow.blocks) {
    if (b.kind !== "worker" || !b.profile) continue;
    const rel = b.profile.replace(/^\.\//, "");
    if (!/^worker-.+\.md$/.test(rel)) continue; // e.g. process.md — S4's, stated residual above
    if (!covered.includes(rel)) uncovered.push(`${b.id} -> ${rel}`);
  }
  assert.deepEqual(
    uncovered,
    [],
    "worker-kind roster block(s) point at worker personas the receipt-check scan did not cover: " +
      uncovered.join(", ")
  );
});

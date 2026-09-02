#!/usr/bin/env node
// Asset-count equality gate for `.github/workflows/release.yml`'s `promote`
// job (#1962).
//
// The comparison lives here, not inline in the workflow step, so a test can
// actually EXECUTE it against fake counts (test/releasepromote.test.ts runs
// N-1 / N / N+1): `promote` only fires on a pushed `vX.Y.Z` tag, so CI can
// never exercise the workflow step itself.
//
// It is an EQUALITY, not a floor (#1962): the old inline `-lt` check refused
// a deficit but let a SURPLUS promote uncounted — a duplicate upload with a
// variant name, or a stray asset from a re-run leg, would ship public
// without anyone counting it (#282 class). Any mismatch leaves the release
// in draft, and the refusal names the direction so a surplus is diagnosed
// as a duplicate-upload problem rather than a missing-leg one.
//
// Dependency-free by design (same rule as scripts/check-versions.js): node
// ships on every CI runner.
const usage = "usage: node scripts/check-release-assets.js <actual-count> <expected-count>";

function main() {
  const argv = process.argv.slice(2);
  if (argv.length !== 2) {
    console.error(usage);
    process.exitCode = 2;
    return;
  }

  const count = Number(argv[0]);
  const expected = Number(argv[1]);
  if (!Number.isInteger(count) || !Number.isInteger(expected) || count < 0 || expected < 0) {
    console.error(`check-release-assets: both counts must be non-negative integers, got ${argv[0]} and ${argv[1]}\n${usage}`);
    process.exitCode = 2;
    return;
  }

  if (count === expected) {
    console.log(`Asset count matches: ${count}/${expected}`);
    return;
  }

  const direction = count < expected
    ? "FEWER than expected — a build matrix leg is missing or failed to upload its assets"
    : "MORE than expected — a duplicate upload or stray asset shipped with the release (#282 class); " +
      "if the extra asset is a legitimately added matrix leg's output, bump EXPECTED_ASSETS_* on release.yml's promote job instead of deleting it";
  console.error(
    `::error::Asset count mismatch: ${count}/${expected} — ${direction}. ` +
      "Refusing to promote; the release stays draft."
  );
  process.exitCode = 1;
}

main();

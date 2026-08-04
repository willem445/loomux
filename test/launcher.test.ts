// Unit tests for the npm launcher's version ordering (issue #815). Run with `npm test`.
//
// These pin the fix for the incident where a stable launcher left on PATH
// reinstalled — downgraded — a newer prerelease install, and the silent installer
// terminated the running app to replace it, killing every agent inside it. The
// launcher is CommonJS under npm/ (its own package.json has no `type`), so it is
// pulled in through createRequire rather than imported.
import { test } from "node:test";
import assert from "node:assert/strict";
import { createRequire } from "node:module";

const require = createRequire(import.meta.url);
const { compareVersions, parseVersion, shouldLaunchExisting } = require("../npm/bin/loomux.js");
const { version: PKG_VERSION } = require("../npm/package.json");

test("parseVersion accepts semver, with or without a v prefix or build metadata", () => {
  assert.deepEqual(parseVersion("1.2.3"), [1, 2, 3, []]);
  assert.deepEqual(parseVersion("v1.2.3"), [1, 2, 3, []]);
  assert.deepEqual(parseVersion("1.1.0-beta9"), [1, 1, 0, ["beta9"]]);
  assert.deepEqual(parseVersion("1.2.3+sha.abc"), [1, 2, 3, []]);
  for (const bad of ["", "1.2", "latest", "1.2.3.4", "not a version"]) {
    assert.equal(parseVersion(bad), null, `${bad} must not parse`);
  }
});

test("compareVersions orders by major, minor, then patch", () => {
  assert.equal(compareVersions("1.0.0", "2.0.0"), -1);
  assert.equal(compareVersions("1.2.0", "1.10.0"), -1, "minor is numeric, not lexical");
  assert.equal(compareVersions("1.0.9", "1.0.10"), -1, "patch is numeric, not lexical");
  assert.equal(compareVersions("2.0.0", "1.9.9"), 1);
  assert.equal(compareVersions("1.2.3", "1.2.3"), 0);
});

test("a prerelease ranks below the release it precedes", () => {
  assert.equal(compareVersions("1.1.0-beta9", "1.1.0"), -1);
  assert.equal(compareVersions("1.1.0", "1.1.0-beta9"), 1);
  assert.equal(compareVersions("1.1.0-beta9", "1.1.0-beta9"), 0);
  // ...but it still outranks any lower release. This is the incident: a 1.0.0
  // launcher must never treat a 1.1.0-beta9 install as something to "upgrade".
  assert.equal(compareVersions("1.1.0-beta9", "1.0.0"), 1);
});

test("prerelease identifiers compare by numeric run, not flat ASCII", () => {
  // The trap this project's tag style walks into: a plain ASCII compare puts
  // beta10 below beta9 ("1" < "9"), which is a downgrade wearing an upgrade's
  // clothes. Both orderings below must hold.
  assert.equal(compareVersions("1.1.0-beta9", "1.1.0-beta10"), -1);
  assert.equal(compareVersions("1.1.0-beta10", "1.1.0-beta9"), 1);
  assert.equal(compareVersions("1.1.0-rc.2", "1.1.0-rc.10"), -1, "dotted numerics too");
  assert.equal(compareVersions("1.1.0-alpha", "1.1.0-beta"), -1, "plain words still sort");
  assert.equal(compareVersions("1.1.0-beta", "1.1.0-beta.1"), -1, "fewer identifiers rank lower");
});

test("compareVersions reports null rather than guessing at an unparseable side", () => {
  assert.equal(compareVersions("garbage", "1.0.0"), null);
  assert.equal(compareVersions("1.0.0", "garbage"), null);
  assert.equal(compareVersions(undefined, "1.0.0"), null);
});

test("shouldLaunchExisting only reinstalls an install that is genuinely older", () => {
  // The incident, in the one direction that matters: an app NEWER than the
  // launcher is launched as-is. Before #815 this returned false and reinstalled,
  // and the install killed the running app.
  assert.equal(shouldLaunchExisting("999.0.0"), true, "newer install → launch as-is");
  assert.equal(shouldLaunchExisting(PKG_VERSION), true, "same version → launch as-is");
  assert.equal(shouldLaunchExisting("0.0.1"), false, "genuinely older → reinstall");
  // Undetectable or unorderable never triggers a download-on-every-launch loop.
  assert.equal(shouldLaunchExisting(null), true, "undetectable → launch as-is");
  assert.equal(shouldLaunchExisting("not-a-version"), true, "unparseable → launch as-is");
});

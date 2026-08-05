// Unit tests for the npm launcher's CLI (issue #845). Run with `npm test`.
//
// #815 left a regression guard here pinning that a plain launch never treated a
// version difference as a reason to reinstall — the silent installer killed the
// running app mid-task. #845 makes that structural: plain `loomux` never
// installs over an existing install, and only the explicit `loomux update`
// command does. These tests pin the command parsing and the AppImage cache
// selection that implement that split. The launcher is CommonJS under npm/ (its
// own package.json has no `type`), so it is pulled in through createRequire.
import { test } from "node:test";
import assert from "node:assert/strict";
import { createRequire } from "node:module";
import { mkdtempSync, writeFileSync, utimesSync, rmSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";

const require = createRequire(import.meta.url);
const { parseArgs, pickCachedAppImage } = require("../npm/bin/loomux.js");
const { version: PKG_VERSION } = require("../npm/package.json");

test("bare `loomux` launches", () => {
  assert.deepEqual(parseArgs([]), { command: "launch" });
});

test("`loomux update` is the only install-over-existing path", () => {
  assert.deepEqual(parseArgs(["update"]), { command: "update" });
  assert.deepEqual(
    parseArgs(["--reinstall"]),
    { command: "update" },
    "the pre-#845 flag stays a compat alias"
  );
});

test("version and help subcommands", () => {
  for (const argv of [["version"], ["--version"]]) {
    assert.deepEqual(parseArgs(argv), { command: "version" }, `${argv} → version`);
  }
  for (const argv of [["help"], ["--help"], ["-h"]]) {
    assert.deepEqual(parseArgs(argv), { command: "help" }, `${argv} → help`);
  }
});

test("unknown input is reported, never guessed at", () => {
  const r = parseArgs(["frobnicate"]);
  assert.equal(r.command, null);
  assert.equal(r.arg, "frobnicate");
  // Trailing junk after a valid command is an error too, not silently dropped.
  assert.equal(parseArgs(["update", "extra"]).command, null);
});

test("pickCachedAppImage prefers the version this launcher ships", () => {
  const dir = mkdtempSync(join(tmpdir(), "loomux-cache-"));
  try {
    writeFileSync(join(dir, "Loomux_1.0.0_amd64.AppImage"), "older build");
    writeFileSync(join(dir, `Loomux_${PKG_VERSION}_amd64.AppImage`), "this launcher's build");
    assert.equal(
      pickCachedAppImage(dir, "amd64", PKG_VERSION),
      join(dir, `Loomux_${PKG_VERSION}_amd64.AppImage`)
    );
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("pickCachedAppImage falls back to the newest cached build", () => {
  const dir = mkdtempSync(join(tmpdir(), "loomux-cache-"));
  try {
    const old = join(dir, "Loomux_1.0.0_amd64.AppImage");
    const newer = join(dir, "Loomux_1.1.0-beta10_amd64.AppImage");
    writeFileSync(old, "old build");
    writeFileSync(newer, "newer build");
    // mtime decides the fallback, so pin it: the old file must sort below even
    // though it was created after (same-second writes).
    utimesSync(old, new Date(2000, 0, 1), new Date(2000, 0, 1));
    assert.equal(pickCachedAppImage(dir, "amd64", "999.0.0"), newer);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("pickCachedAppImage ignores other platforms and non-AppImage files", () => {
  const dir = mkdtempSync(join(tmpdir(), "loomux-cache-"));
  try {
    writeFileSync(join(dir, "Loomux_1.0.0_aarch64.AppImage"), "arm build");
    writeFileSync(join(dir, "note.txt"), "irrelevant");
    assert.equal(pickCachedAppImage(dir, "amd64", PKG_VERSION), null);
    assert.equal(
      pickCachedAppImage(dir, "aarch64", PKG_VERSION),
      join(dir, "Loomux_1.0.0_aarch64.AppImage"),
      "a matching arch still resolves"
    );
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

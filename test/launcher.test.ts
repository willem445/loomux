// Unit tests for the npm launcher (issues #815, #816, #845). Run with `npm test`.
//
// Two properties are pinned here, and both are properties whose failure costs a
// running app plus every agent inside it:
//
//   1. Plain `loomux` never installs over an existing install; only the
//      explicit `loomux update` does (#845). The silent installer terminates a
//      running Loomux to replace its files, which is what #815 actually was.
//   2. `loomux update` is channel-aware and never downgrades (#816). It picks
//      the newest release on the installed build's channel by semver ordering,
//      and refuses outright when that is older than what is installed.
//
// Both live in pure exported functions specifically so a test can distinguish
// them: `planAction` is the whole launch-or-install decision, `updateVerdict`
// the whole update decision. The launcher is CommonJS under npm/ (its own
// package.json has no `type`), so it is pulled in through createRequire.
import { test } from "node:test";
import assert from "node:assert/strict";
import { createRequire } from "node:module";
import { mkdtempSync, writeFileSync, utimesSync, rmSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";

const require = createRequire(import.meta.url);
const {
  parseArgs,
  planAction,
  parseVersion,
  compareVersions,
  channelOf,
  newestOnChannel,
  updateVerdict,
  updateBaseline,
  newestVersion,
  installedWindowsVersion,
  pickCachedAppImage,
  appImageVersion,
} = require("../npm/bin/loomux.js");
const { version: PKG_VERSION } = require("../npm/package.json");

// ---------- command parsing ----------

test("bare `loomux` launches", () => {
  assert.deepEqual(parseArgs([]), { command: "launch" });
});

test("`loomux update` is the only install-over-existing path", () => {
  assert.deepEqual(parseArgs(["update"]), { command: "update" });
  assert.deepEqual(
    parseArgs(["--reinstall"]),
    { command: "update", deprecated: "--reinstall" },
    "the pre-#845 flag stays a compat alias, and reports itself so main() can say so"
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
  // Trailing junk after a valid command is an error too, not silently dropped —
  // and the offending token is the junk, not the command.
  assert.deepEqual(parseArgs(["update", "extra"]), { command: null, arg: "extra" });
  assert.deepEqual(parseArgs(["bogus", "extra"]), { command: null, arg: "bogus" });
});

// ---------- launch vs install ----------

test("plain `loomux` never installs over an existing install", () => {
  // THE #815 property. Before #845 a plain launch could decide to reinstall;
  // the silent installer then killed the running app and every agent in it.
  // A launcher that forces the install path on a plain launch fails here.
  assert.equal(planAction("launch", true), "launch", "something installed → launch it");
  assert.equal(
    planAction("launch", false),
    "install",
    "nothing installed → first install is the only way to launch anything"
  );
});

test("`loomux update` installs even when something is already installed", () => {
  // The other direction: an update that declines to install because something
  // is already there is a silent no-op, and `update` is the ONLY path that can
  // still fetch once an install exists.
  assert.equal(planAction("update", true), "install", "update over an install → install");
  assert.equal(planAction("update", false), "install", "update with nothing there → install");
});

// ---------- version ordering (#815) ----------

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
  // The live topology this repo actually has: v1.0.0 is NEWER than v0.10.0,
  // even though GitHub's `latest` pointer currently says otherwise.
  assert.equal(compareVersions("v1.0.0", "v0.10.0"), 1);
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

test("channelOf splits stable from prerelease", () => {
  assert.equal(channelOf("1.1.0"), "stable");
  assert.equal(channelOf("v1.0.0"), "stable");
  assert.equal(channelOf("1.1.0-beta11"), "prerelease");
  assert.equal(channelOf("1.1.0-rc.1"), "prerelease");
  // Unparseable is treated as prerelease — the permissive side. `update` never
  // orders against an unparseable version anyway (currentVersion falls back to
  // the launcher's own), so this only decides which releases are candidates.
  assert.equal(channelOf("garbage"), "prerelease");
});

// ---------- update resolution (#816 / #846) ----------

// This repo's release list as it actually stands, newest-first — the shape the
// GitHub API returns. v1.0.0 is stable and NEWER than v0.10.0, yet
// `/releases/latest` resolves v0.10.0 (the make_latest pointer never moved; see
// release.yml's #341/#543 note). Every case below is a real user of this repo.
const RELEASES = [
  { tag_name: "v1.1.0-beta11", prerelease: true, draft: false },
  { tag_name: "v1.1.0-beta10", prerelease: true, draft: false },
  { tag_name: "v1.1.0-beta9", prerelease: true, draft: false },
  { tag_name: "v1.0.0", prerelease: false, draft: false },
  { tag_name: "v0.10.0", prerelease: false, draft: false },
  { tag_name: "v0.10.0-beta", prerelease: true, draft: false },
  { tag_name: "v0.9.0", prerelease: false, draft: false },
];

test("update never resolves GitHub's `latest` pointer — a beta install gets the newest beta", () => {
  // THE headline regression. Resolving /releases/latest here hands a
  // 1.1.0-beta11 install v0.10.0: eleven releases back, announced as an update,
  // installed silently. The newest build on the prerelease channel is beta11 —
  // which is what is already installed, so this is a reinstall, not a downgrade.
  const v = updateVerdict(RELEASES, "1.1.0-beta11");
  assert.equal(v.release.tag_name, "v1.1.0-beta11");
  assert.equal(v.channel, "prerelease");
  assert.equal(v.action, "reinstall");
});

test("update never resolves GitHub's `latest` pointer — a stable install gets the newest stable", () => {
  // The same bug reaches stable users too, because the pointer is wrong in the
  // ordering sense, not just the channel sense: v1.0.0 is stable and newer than
  // v0.10.0, and /releases/latest still says v0.10.0.
  const v = updateVerdict(RELEASES, "1.0.0");
  assert.equal(v.release.tag_name, "v1.0.0", "v0.10.0 must never win over v1.0.0");
  assert.equal(v.action, "reinstall");
});

test("a stable install is never handed a prerelease it did not opt into", () => {
  const v = updateVerdict(RELEASES, "0.9.0");
  assert.equal(v.channel, "stable");
  assert.equal(v.release.tag_name, "v1.0.0", "newest STABLE, not newest overall");
  assert.equal(v.action, "install");
});

test("a prerelease install takes the newest release of either kind", () => {
  const v = updateVerdict(RELEASES, "1.1.0-beta9");
  assert.equal(v.channel, "prerelease");
  assert.equal(v.release.tag_name, "v1.1.0-beta11", "beta10 must not beat beta11");
  assert.equal(v.action, "install");
});

test("update refuses outright when the newest release on the channel is older", () => {
  // #816's "no downgrades" half, on the one path that can still reach an
  // installer. Every route back to this is still open — a stale launcher on
  // PATH, a re-pointed make_latest, a yanked release — so the guard is on the
  // verdict, not on the endpoint.
  const stale = RELEASES.filter((r) => r.tag_name !== "v1.1.0-beta11");
  const v = updateVerdict(stale, "1.1.0-beta11");
  assert.equal(v.action, "refuse", "beta10 must not be installed over a beta11 install");
  assert.equal(v.release.tag_name, "v1.1.0-beta10");
  // And the pointer's own answer is refused just as hard.
  assert.equal(updateVerdict([RELEASES[4]], "1.1.0-beta11").action, "refuse");
  assert.equal(updateVerdict([RELEASES[4]], "1.0.0").action, "refuse");
});

test("update ignores drafts and mislabelled prereleases", () => {
  const withDraft = [
    { tag_name: "v2.0.0", prerelease: false, draft: true },
    // Flagged stable by hand but tagged as a prerelease: the tag wins, so a
    // stable install is not handed it.
    { tag_name: "v1.2.0-rc.1", prerelease: false, draft: false },
    ...RELEASES,
  ];
  assert.equal(
    updateVerdict(withDraft, "1.0.0").release.tag_name,
    "v1.0.0",
    "a draft is not a release, and an rc tag is not stable"
  );
  assert.equal(
    updateVerdict(withDraft, "1.1.0-beta11").release.tag_name,
    "v1.2.0-rc.1",
    "the prerelease channel does take it, and it outranks beta11"
  );
});

test("newestOnChannel skips tags it cannot order rather than guessing", () => {
  const messy = [{ tag_name: "nightly", prerelease: false, draft: false }, ...RELEASES];
  assert.equal(newestOnChannel(messy, "1.0.0").tag_name, "v1.0.0");
  assert.equal(newestOnChannel([{ tag_name: "nightly", draft: false }], "1.0.0"), null);
  assert.deepEqual(updateVerdict([], "1.0.0"), {
    action: "none",
    channel: "stable",
    current: "1.0.0",
    release: null,
  });
});

// ---------- what update orders against ----------

// `reg query` output, as Windows actually prints it.
const regOut = (v: string) =>
  `\r\nHKEY_LOCAL_MACHINE\\Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\Loomux\r\n    DisplayVersion    REG_SZ    ${v}\r\n\r\n`;

/** A fake `reg` that answers only for the hive/view pairs in `present`. */
const fakeReg = (present: Record<string, string>) => (key: string, view: string | null) =>
  present[`${key.split("\\")[0]}${view ? " " + view : ""}`] ?? "";

test("a per-machine install is readable — HKCU alone is not the whole registry", () => {
  // findWindowsExe already looks under %PROGRAMFILES%, which is a per-machine
  // NSIS install, and those register under HKLM. Probing HKCU only made every
  // one of them read as "no version", which updateBaseline then has to refuse —
  // so the probe must actually cover them or `update` is broken for that user.
  assert.equal(
    installedWindowsVersion(fakeReg({ "HKLM /reg:64": regOut("1.1.0-beta11") })),
    "1.1.0-beta11",
    "a per-machine install in the native view must be found"
  );
  assert.equal(
    installedWindowsVersion(fakeReg({ "HKLM /reg:32": regOut("1.1.0-beta11") })),
    "1.1.0-beta11",
    "...and one whose keys landed in the WOW6432Node view"
  );
  assert.equal(
    installedWindowsVersion(fakeReg({ HKCU: regOut("1.0.0") })),
    "1.0.0",
    "per-user installs keep working"
  );
  assert.equal(installedWindowsVersion(fakeReg({})), null, "no key anywhere → unknown");
});

test("when two installs exist, the guard orders against the newest", () => {
  // If ANY install on the machine is newer than the release we resolved,
  // installing that release downgrades it. So a stale per-machine leftover must
  // not unblock a downgrade of a newer per-user install, or the reverse.
  assert.equal(
    installedWindowsVersion(
      fakeReg({ HKCU: regOut("1.1.0-beta11"), "HKLM /reg:64": regOut("0.10.0") })
    ),
    "1.1.0-beta11"
  );
  assert.equal(
    installedWindowsVersion(
      fakeReg({ HKCU: regOut("0.10.0"), "HKLM /reg:32": regOut("1.1.0-beta11") })
    ),
    "1.1.0-beta11"
  );
  assert.equal(newestVersion(["1.0.0", "garbage", "1.1.0-beta9"]), "1.1.0-beta9");
  assert.equal(newestVersion(["garbage"]), null, "nothing orderable → unknown");
  assert.equal(newestVersion([]), null);
});

test("an install whose version cannot be read stops the update — it is not a default", () => {
  // THE fail-closed property. Substituting the launcher's own version here
  // silently disarms the whole guard for anyone the probe cannot read.
  assert.equal(updateBaseline(true, null), null, "installed but unreadable → refuse");
  assert.equal(updateBaseline(true, "not-a-version"), null, "...and unparseable → refuse");
  // The two cases that are genuinely safe stay unaffected.
  assert.equal(updateBaseline(true, "1.1.0-beta11"), "1.1.0-beta11", "detected → order against it");
  assert.equal(
    updateBaseline(false, null),
    PKG_VERSION,
    "nothing installed → nothing to downgrade, so the launcher picks the channel"
  );
});

test("a per-machine beta install under a stale stable launcher is not downgraded", () => {
  // The end-to-end scenario the two mutants above combine into, and the one
  // that reached a real user: Loomux 1.1.0-beta11 installed per-machine (HKLM),
  // a stale 0.10.0 launcher on PATH. Reading HKCU only returned null; treating
  // null as "use the launcher's version" then resolved the STABLE channel and
  // installed v1.0.0 over the beta — a downgrade AND a channel switch, silent.
  const detected = installedWindowsVersion(fakeReg({ "HKLM /reg:64": regOut("1.1.0-beta11") }));
  const current = updateBaseline(true, detected);
  assert.equal(current, "1.1.0-beta11", "the per-machine install is what we order against");
  const v = updateVerdict(RELEASES, current);
  assert.equal(v.channel, "prerelease", "not the launcher's stable channel");
  assert.equal(v.release.tag_name, "v1.1.0-beta11");
  assert.notEqual(v.release.tag_name, "v1.0.0", "the downgrade must not be reachable");
  assert.equal(v.action, "reinstall");
});

// ---------- Linux AppImage cache ----------

test("pickCachedAppImage picks the newest cached build, even one newer than the launcher", () => {
  const dir = mkdtempSync(join(tmpdir(), "loomux-cache-"));
  try {
    const launcherVer = join(dir, `Loomux_${PKG_VERSION}_amd64.AppImage`);
    const newer = join(dir, "Loomux_999.0.0_amd64.AppImage");
    writeFileSync(launcherVer, "this launcher's own build");
    writeFileSync(newer, "a build an earlier `loomux update` installed");
    // mtime decides, so pin it: the launcher-matched file must lose despite
    // being written second (same-second writes).
    utimesSync(launcherVer, new Date(2000, 0, 1), new Date(2000, 0, 1));
    assert.equal(pickCachedAppImage(dir, "amd64"), newer);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("pickCachedAppImage returns null when nothing is cached", () => {
  const dir = mkdtempSync(join(tmpdir(), "loomux-cache-"));
  try {
    assert.equal(pickCachedAppImage(dir, "amd64"), null);
    writeFileSync(join(dir, "Loomux_1.0.0_aarch64.AppImage"), "arm build");
    writeFileSync(join(dir, "note.txt"), "irrelevant");
    assert.equal(
      pickCachedAppImage(dir, "amd64"),
      null,
      "other arch and non-AppImage files are ignored"
    );
    assert.equal(
      pickCachedAppImage(dir, "aarch64"),
      join(dir, "Loomux_1.0.0_aarch64.AppImage"),
      "a matching arch still resolves"
    );
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("appImageVersion reads the version Linux has no other record of", () => {
  // Linux has no installer and no registry, so the cached asset name is the
  // only thing the downgrade guard can order against.
  assert.equal(
    appImageVersion("/home/u/.cache/loomux/Loomux_1.1.0-beta11_amd64.AppImage"),
    "1.1.0-beta11"
  );
  assert.equal(appImageVersion("Loomux_1.0.0_aarch64.AppImage"), "1.0.0");
  assert.equal(appImageVersion(null), null, "nothing cached → no version to order against");
  assert.equal(appImageVersion("Loomux.AppImage"), null, "an unrecognised name is not a version");
});

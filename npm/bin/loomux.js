#!/usr/bin/env node
// Loomux npm launcher.
//
//   npm install -g loomux   # then run `loomux`
//   npx loomux              # download + launch in one shot
//
// Loomux itself is a native (Tauri) desktop app, not a JS program — so this
// package ships no binary. Instead it fetches the matching GitHub release
// asset for the host platform, installs/caches it, and launches it. The
// per-platform logic mirrors install.sh / install.ps1 so all three install
// paths behave the same.
//
// The command has three subcommands (#845):
//
//   loomux            launch the installed app; install it only if missing
//   loomux update     install/refresh the app from the newest release on the
//                     installed build's channel — the only path that fetches
//                     when something is already installed or cached
//   loomux version    print this launcher's version
//   loomux help       print usage
//
// Plain `loomux` deliberately never upgrades: the silent installers terminate
// a running Loomux to replace its files, so autoupdate killed live agents
// (#815) and the update decision belongs to the human as `loomux update`.
//
// `loomux update` is channel-aware and never downgrades (#815/#816/#846) —
// see the "update resolution" section for why both halves are load-bearing.
//
// Dependency-free on purpose: `npx loomux` should have nothing to compile
// and nothing to trust beyond Node's own stdlib (Node >=18 for global fetch).

"use strict";

const os = require("os");
const fs = require("fs");
const path = require("path");
const { spawn, spawnSync } = require("child_process");

const REPO = "willem445/loomux";
const { version: PKG_VERSION, name: PKG_NAME } = require("../package.json");

const BLUE = "\x1b[1;34m";
const GREEN = "\x1b[1;32m";
const RED = "\x1b[1;31m";
const RESET = "\x1b[0m";
const tty = process.stderr.isTTY;
const paint = (c, s) => (tty ? `${c}${s}${RESET}` : s);

function say(msg) {
  process.stderr.write(`${paint(BLUE, "loomux")} ${msg}\n`);
}
function die(msg) {
  process.stderr.write(`${paint(RED, "loomux")} ${msg}\n`);
  process.exit(1);
}

// ---------- CLI ----------

const HELP = `loomux ${PKG_VERSION} — Loomux desktop launcher

Launches the Loomux desktop app (installing it first if needed). Run
\`loomux\` with no arguments to launch.

USAGE
  loomux            Launch the installed app, or install it if missing. Never
                    updates an existing install.
  loomux update     Install/refresh the app from the newest GitHub release on
                    your channel: a stable install stays on stable, a beta
                    install takes the newest build of either kind. Never
                    downgrades. On Windows and macOS the launcher refuses while
                    Loomux is running — the installer would close the running app.
  loomux version    Print this launcher's version.
  loomux help       Show this help.
  loomux --help, -h Same as \`loomux help\`.
  loomux --version  Same as \`loomux version\`.

Requires Node 18+.
`;

// Resolve argv to a command. Any input we don't recognize — including trailing
// junk after a valid command — comes back as `{command:null}` so main() dies
// with a hint instead of guessing what the user meant.
//
// The desktop app takes no argv of its own (src-tauri/src/main.rs is a bare
// `loomux_lib::run()`), so there is nothing to forward and every extra token is
// a mistake worth reporting rather than passing along.
function parseArgs(argv) {
  if (argv.length === 0) return { command: "launch" };
  if (argv.length !== 1) {
    // A valid command followed by junk reports the junk, not the command.
    const KNOWN = new Set(["help", "--help", "-h", "version", "--version", "update", "--reinstall"]);
    return { command: null, arg: KNOWN.has(argv[0]) ? argv[1] : argv[0] };
  }
  switch (argv[0]) {
    case "help":
    case "--help":
    case "-h":
      return { command: "help" };
    case "version":
    case "--version":
      return { command: "version" };
    case "update":
      return { command: "update" };
    // Compat alias for pre-#845 scripts and muscle memory. Reported back so
    // main() can print a deprecation line — the meaning shifted (it used to
    // install the launcher's own matching tag) and a silent alias would hide
    // that from anyone who scripted it.
    case "--reinstall":
      return { command: "update", deprecated: "--reinstall" };
    default:
      return { command: null, arg: argv[0] };
  }
}

// ---------- launch-or-install decision ----------

// The whole "launch what's there, or run an installer?" decision, in one pure
// place so it can be pinned by a test — the #815 failure mode lived exactly
// here, and a wrong answer costs a running app and every agent inside it.
//
//   plain `loomux` + something installed  -> launch it, never install (#815)
//   plain `loomux` + nothing installed    -> install (first run)
//   `loomux update`                       -> install, always (that is the ask)
//
// Kept as one function rather than an `existing && !force` repeated in each
// platform runner: three copies of a safety rule is three places to get it
// wrong, and none of them were testable.
function planAction(command, hasExisting) {
  const force = command === "update";
  if (hasExisting && !force) return "launch";
  return "install";
}

// ---------- GitHub release lookup ----------

async function ghJson(url) {
  const res = await fetch(url, {
    headers: {
      "User-Agent": "loomux-npm-launcher",
      Accept: "application/vnd.github+json",
    },
  });
  if (!res.ok) {
    const err = new Error(`GitHub API ${res.status} for ${url}`);
    err.status = res.status;
    throw err;
  }
  return res.json();
}

// Prefer the release matching this package's version (so `npx loomux@X`
// installs app vX); fall back to whatever the latest release is. Used by plain
// launch's first install only — that path only ever runs when there is nothing
// installed, so it cannot downgrade anything.
async function resolveRelease() {
  try {
    return await ghJson(
      `https://api.github.com/repos/${REPO}/releases/tags/v${PKG_VERSION}`
    );
  } catch (e) {
    if (e.status !== 404) throw e;
    say(`no release tagged v${PKG_VERSION} yet — using the latest release`);
    return ghJson(`https://api.github.com/repos/${REPO}/releases/latest`);
  }
}

// ---------- version ordering ----------

// Split a semver-ish string into [major, minor, patch, prerelease[]], or null if
// it doesn't parse. Build metadata (`+…`) is ignored, as semver requires.
function parseVersion(v) {
  const m = /^v?(\d+)\.(\d+)\.(\d+)(?:-([0-9A-Za-z.-]+))?(?:\+[0-9A-Za-z.-]+)?$/.exec(
    String(v).trim()
  );
  if (!m) return null;
  return [Number(m[1]), Number(m[2]), Number(m[3]), m[4] ? m[4].split(".") : []];
}

// Compare two prerelease identifiers. Semver says numeric identifiers compare
// numerically and alphanumeric ones compare as ASCII — but this project tags
// `beta9`, `beta10`, where a flat ASCII compare puts beta10 BELOW beta9 ("1" <
// "9") and would hand us a downgrade that reads as an upgrade. So each
// identifier is compared as alternating runs of digits and non-digits, digits
// numerically: "beta9" < "beta10", and plain semver forms are unaffected.
function compareIdentifier(a, b) {
  const runs = (s) => s.match(/\d+|\D+/g) || [];
  const [ra, rb] = [runs(a), runs(b)];
  for (let i = 0; i < Math.max(ra.length, rb.length); i++) {
    const [x, y] = [ra[i], rb[i]];
    if (x === undefined) return -1;
    if (y === undefined) return 1;
    const [nx, ny] = [/^\d+$/.test(x), /^\d+$/.test(y)];
    if (nx && ny) {
      if (Number(x) !== Number(y)) return Number(x) < Number(y) ? -1 : 1;
    } else if (nx !== ny) {
      return nx ? -1 : 1; // numeric identifiers rank below alphanumeric ones
    } else if (x !== y) {
      return x < y ? -1 : 1;
    }
  }
  return 0;
}

// -1 / 0 / 1 for a < b, a == b, a > b; null when either side doesn't parse.
function compareVersions(a, b) {
  const [pa, pb] = [parseVersion(a), parseVersion(b)];
  if (!pa || !pb) return null;
  for (let i = 0; i < 3; i++) if (pa[i] !== pb[i]) return pa[i] < pb[i] ? -1 : 1;
  // A prerelease ranks below the release it precedes: 1.1.0-beta9 < 1.1.0.
  if (pa[3].length === 0 || pb[3].length === 0) {
    if (pa[3].length === pb[3].length) return 0;
    return pa[3].length ? -1 : 1;
  }
  for (let i = 0; i < Math.max(pa[3].length, pb[3].length); i++) {
    const [x, y] = [pa[3][i], pb[3][i]];
    if (x === undefined) return -1; // fewer identifiers ranks lower
    if (y === undefined) return 1;
    const c = compareIdentifier(x, y);
    if (c !== 0) return c;
  }
  return 0;
}

// ---------- update resolution ----------

// Which release channel a version sits on. A version with no prerelease tag is
// stable; anything else (`1.1.0-beta11`, `1.1.0-rc.1`) is a prerelease.
function channelOf(v) {
  const p = parseVersion(v);
  return p && p[3].length === 0 ? "stable" : "prerelease";
}

// A release counts as a prerelease if GitHub says so OR if its tag carries a
// prerelease identifier. Two sources because the flag is set by hand at publish
// time and has been wrong on this repo before (release.yml re-asserts it on
// re-runs for exactly that reason); the tag is what the version ordering below
// actually uses, so they must not disagree in the permissive direction.
function isPrereleaseRelease(r) {
  return Boolean(r.prerelease) || channelOf(r.tag_name) === "prerelease";
}

// The newest release on `current`'s channel, ordered by semver — deliberately
// NOT by GitHub's `/releases/latest`.
//
// `/releases/latest` is not "the newest stable release": it is the mutable
// `make_latest` pointer, and on this repo it is wrong *right now*. It resolves
// v0.10.0 (published 2026-07-16) while the newer, non-prerelease v1.0.0
// (2026-07-27) sits above it — the promote-time failure documented at length in
// .github/workflows/release.yml (#341/#543), where v1.0.0 published cleanly but
// never took the pointer. Resolving `update` through that endpoint downgrades
// every 1.x user: eleven releases back for a 1.1.0-beta11 install, and still a
// downgrade for a stable v1.0.0 one. So the ordering is computed here, from the
// full list, and the pointer is never consulted.
//
// Channel, not "newest overall": a stable install must never be handed a beta
// it did not opt into. A prerelease install considers everything, because this
// repo's newest build is always a prerelease — refusing them would make
// `update` a permanent no-op for the entire beta train, which is the bug that
// motivated the change in the first place.
//
// One page, newest-first: this repo has 24 releases and the API caps per_page
// at 100. If it ever exceeds 100 the newest of each channel is still on page 1
// unless >100 consecutive prereleases follow the newest stable, which would
// mean a release cadence this launcher is the least of our problems for.
function newestOnChannel(releases, current) {
  const wantStable = channelOf(current) === "stable";
  let best = null;
  for (const r of releases || []) {
    if (!r || r.draft) continue;
    if (wantStable && isPrereleaseRelease(r)) continue;
    if (!parseVersion(r.tag_name)) continue; // a tag we can't order can't win
    if (!best || compareVersions(r.tag_name, best.tag_name) > 0) best = r;
  }
  return best;
}

// The entire `loomux update` decision, pure so it can be pinned by tests:
//
//   {action:"install"}   a genuinely newer build on this channel — go
//   {action:"reinstall"} the newest build IS what's installed — reinstall it
//                        (this is the repair case `--reinstall` always meant)
//   {action:"refuse"}    the newest build on this channel is OLDER than what is
//                        installed — never install it (#816's "no downgrades")
//   {action:"none"}      no orderable release on this channel at all
//
// "refuse", not "warn": #815 was a downgrade that announced itself as an
// upgrade and killed a running app, and every route back to it is still open —
// a stale launcher on PATH, a re-pointed `make_latest`, a yanked release. The
// endpoint fix above removes today's instance; this removes the class.
function updateVerdict(releases, current) {
  const channel = channelOf(current);
  const release = newestOnChannel(releases, current);
  if (!release) return { action: "none", channel, current, release: null };
  const cmp = compareVersions(release.tag_name, current);
  const action = cmp === 0 ? "reinstall" : cmp < 0 ? "refuse" : "install";
  return { action, channel, current, release };
}

// What `loomux update` orders against: the version of the app actually
// installed, since that is the thing an installer would overwrite. Falls back to
// this launcher's own version when nothing is installed (a first install) or the
// probe returned something unorderable. The fallback always parses, so neither
// the channel choice nor the refusal above is ever made against a version we
// could not read.
function currentVersion(installed) {
  if (installed && parseVersion(installed)) return installed;
  if (installed) {
    say(
      `can't order the installed version ("${installed}") — using this ` +
        `launcher's v${PKG_VERSION} to pick the update instead`
    );
  }
  return PKG_VERSION;
}

async function resolveUpdateRelease(installed) {
  const current = currentVersion(installed);
  const releases = await ghJson(
    `https://api.github.com/repos/${REPO}/releases?per_page=100`
  );
  const v = updateVerdict(releases, current);
  if (v.action === "none") {
    die(`no installable ${v.channel} release found for this repo`);
  }
  if (v.action === "refuse") {
    die(
      `refusing to downgrade: the newest ${v.channel} release is ` +
        `${v.release.tag_name}, older than the installed v${v.current}. ` +
        (v.channel === "stable"
          ? `Installing it would replace a newer build with an older one. If you meant to move to a prerelease, install one from the releases page.`
          : `Update this launcher first (\`npm i -g ${PKG_NAME}@latest\`) if you expected something newer.`)
    );
  }
  if (v.action === "reinstall") {
    say(`already on ${v.release.tag_name} (newest ${v.channel}) — reinstalling it`);
  } else {
    say(`updating v${v.current} → ${v.release.tag_name} (newest ${v.channel})`);
  }
  return v.release;
}

/** First asset whose name matches `re`, or null. */
function pickAsset(release, re) {
  const assets = release.assets || [];
  return assets.find((a) => re.test(a.name)) || null;
}

async function download(url, dest) {
  const res = await fetch(url, { headers: { "User-Agent": "loomux-npm-launcher" } });
  if (!res.ok || !res.body) die(`download failed (${res.status}): ${url}`);
  fs.mkdirSync(path.dirname(dest), { recursive: true });
  const buf = Buffer.from(await res.arrayBuffer());
  try {
    fs.writeFileSync(dest, buf);
  } catch (e) {
    // Linux has no running-instance probe (loomuxIsRunning is false there), so
    // the kernel is the one that catches "you are overwriting a running
    // AppImage" — as a raw ETXTBSY errno. Translate it into the same advice
    // refuseIfRunning() gives on the other two platforms.
    if (e && e.code === "ETXTBSY") {
      die(
        `${path.basename(dest)} is running — refusing to overwrite it. Quit ` +
          `that Loomux window, then run this again.`
      );
    }
    throw e;
  }
}

function cacheDir() {
  const base =
    process.platform === "win32"
      ? process.env.LOCALAPPDATA || os.tmpdir()
      : process.env.XDG_CACHE_HOME || path.join(os.homedir(), ".cache");
  return path.join(base, "loomux");
}

// ---------- Linux AppImage cache ----------

// The newest cached AppImage for this platform wins, on every platform's rule.
// On Linux the cached AppImage IS the install, so plain `loomux` launches the
// newest cached build and never downloads — `loomux update` is the only path
// that fetches again (#845). Download recency is a fine stand-in for version
// recency: cache files are only ever created by a download, never touched
// after. Deliberately no "prefer the launcher's own version" bias — update can
// install a build NEWER than the launcher, and plain launch must surface it.
//
// stat once per file into a pair, then sort: statting inside the comparator ran
// it O(n log n) times, and threw uncaught if a cache file vanished between the
// readdir and the sort.
function pickCachedAppImage(dir, suffix) {
  let entries;
  try {
    entries = fs.readdirSync(dir, { withFileTypes: true });
  } catch {
    return null;
  }
  const re = new RegExp(`^Loomux_.+_${suffix}\\.AppImage$`);
  const hits = [];
  for (const e of entries) {
    if (!e.isFile() || !re.test(e.name)) continue;
    const file = path.join(dir, e.name);
    try {
      hits.push({ file, mtime: fs.statSync(file).mtimeMs });
    } catch {
      // Vanished between readdir and stat — it is not a launchable install.
    }
  }
  hits.sort((a, b) => b.mtime - a.mtime);
  return hits.length ? hits[0].file : null;
}

// The version of a cached AppImage, read off its filename: Linux has no
// installer and no registry, so the asset name is the only version record there
// is. `Loomux_1.1.0-beta11_amd64.AppImage` → "1.1.0-beta11".
function appImageVersion(file) {
  if (!file) return null;
  const m = /^Loomux_(.+)_(?:amd64|aarch64)\.AppImage$/.exec(path.basename(file));
  return m ? m[1] : null;
}

// ---------- running-instance guard ----------

// Is a Loomux desktop app running right now? Installing over one is what makes
// the launcher lethal: the silent installer terminates the running process to
// replace its files, taking down the app and anything running inside it.
//
// Unknown (no probe, or the probe failed) is reported as "not running": on both
// platforms the probe ships with the OS, so a failure means something exotic, and
// a launcher that refuses to install on such a machine is a worse bug than one
// that installs. Plain launch only reaches this guard when it found nothing to
// launch and must install; its real job is protecting `loomux update` from an
// install-over-running-app.
function loomuxIsRunning() {
  if (process.platform === "win32") {
    // A filter that matches nothing still exits 0 ("INFO: No tasks…"), so test
    // the output rather than the status.
    const out = spawnSync("tasklist", ["/FI", "IMAGENAME eq Loomux.exe", "/NH"], {
      encoding: "utf8",
    });
    return /Loomux\.exe/i.test(out.stdout || "");
  }
  if (process.platform === "darwin") {
    const out = spawnSync("pgrep", ["-x", "Loomux"], { encoding: "utf8" });
    return out.status === 0 && (out.stdout || "").trim() !== "";
  }
  // Linux runs an AppImage in place; nothing is replaced under it, and an
  // overwrite of the running image is caught as ETXTBSY in download().
  return false;
}

// Refuse to install while the app is running — including under `loomux update`,
// which is an explicit ask to reinstall, not consent to kill a live instance.
// Quitting first is always the user's call to make, never the launcher's.
function refuseIfRunning() {
  if (!loomuxIsRunning()) return;
  die(
    "Loomux is running — refusing to install over it. The installer would " +
      "terminate the running app to replace its files, closing every window and " +
      "anything running inside it. Quit Loomux, then run this again."
  );
}

// ---------- platform installers ----------

async function runLinux(getRelease, command) {
  const arch = process.arch;
  const suffix = arch === "arm64" ? "aarch64" : arch === "x64" ? "amd64" : null;
  if (!suffix) die(`unsupported Linux architecture: ${arch}`);

  // Plain launch reuses whatever AppImage is cached — never a fresh download —
  // and `update` forces one.
  const cached = pickCachedAppImage(cacheDir(), suffix);
  if (planAction(command, Boolean(cached)) === "launch") {
    say(`launching ${path.basename(cached)}`);
    const child = spawn(cached, [], { detached: true, stdio: "ignore" });
    child.unref();
    return;
  }

  const release = await getRelease(appImageVersion(cached));
  const asset = pickAsset(release, new RegExp(`_${suffix}\\.AppImage$`));
  if (!asset) die(`no Linux (${arch}) AppImage in release ${release.tag_name}`);
  const dest = path.join(cacheDir(), asset.name);
  say(`downloading ${asset.name}`);
  await download(asset.browser_download_url, dest);
  fs.chmodSync(dest, 0o755);
  say(`launching ${path.basename(dest)}`);
  // Detach so the GUI outlives this short-lived launcher process.
  const child = spawn(dest, [], { detached: true, stdio: "ignore" });
  child.unref();
}

// The version macOS recorded for the installed bundle.
function installedMacVersion() {
  const out = spawnSync(
    "defaults",
    ["read", "/Applications/Loomux.app/Contents/Info", "CFBundleShortVersionString"],
    { encoding: "utf8" }
  );
  if (out.status !== 0 || !out.stdout) return null;
  return out.stdout.trim() || null;
}

async function runMac(getRelease, command) {
  const appPath = "/Applications/Loomux.app";
  const existing = fs.existsSync(appPath);
  if (planAction(command, existing) === "launch") {
    say("launching installed Loomux.app");
    spawnSync("open", ["-a", "Loomux"], { stdio: "ignore" });
    return;
  }

  refuseIfRunning();
  const release = await getRelease(existing ? installedMacVersion() : null);
  const re = process.arch === "arm64" ? /_aarch64\.dmg$/ : /_x64\.dmg$/;
  const asset = pickAsset(release, re);
  if (!asset) die(`no macOS (${process.arch}) build in release ${release.tag_name}`);

  const dmg = path.join(os.tmpdir(), asset.name);
  say(`downloading ${asset.name}`);
  await download(asset.browser_download_url, dmg);

  say("installing to /Applications");
  const attach = spawnSync(
    "hdiutil",
    ["attach", "-nobrowse", "-readonly", dmg],
    { encoding: "utf8" }
  );
  if (attach.status !== 0) die("could not mount the disk image");
  // Last whitespace-separated field of the last line is the mount point.
  const lines = attach.stdout.trim().split("\n");
  const mount = lines[lines.length - 1].split("\t").pop().trim();

  try {
    spawnSync("rm", ["-rf", appPath]);
    const cp = spawnSync("cp", ["-R", path.join(mount, "Loomux.app"), "/Applications/"]);
    if (cp.status !== 0) die("could not copy Loomux.app into /Applications");
  } finally {
    spawnSync("hdiutil", ["detach", mount, "-quiet"]);
    fs.rmSync(dmg, { force: true });
  }
  // Builds are unsigned; clear quarantine so Gatekeeper won't flag it.
  spawnSync("xattr", ["-cr", appPath]);
  say("launching Loomux.app");
  spawnSync("open", ["-a", "Loomux"], { stdio: "ignore" });
}

// Common install locations for the Tauri NSIS build (per-user by default).
function findWindowsExe() {
  const candidates = [
    path.join(process.env.LOCALAPPDATA || "", "Programs", "Loomux", "Loomux.exe"),
    path.join(process.env.LOCALAPPDATA || "", "Loomux", "Loomux.exe"),
    path.join(process.env.PROGRAMFILES || "", "Loomux", "Loomux.exe"),
  ];
  return candidates.find((p) => p && fs.existsSync(p)) || null;
}

// Tauri's NSIS installer records the version it installed (per-user, HKCU).
function installedWindowsVersion() {
  const out = spawnSync(
    "reg",
    [
      "query",
      "HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\Loomux",
      "/v",
      "DisplayVersion",
    ],
    { encoding: "utf8" }
  );
  if (out.status !== 0 || !out.stdout) return null;
  const m = out.stdout.match(/DisplayVersion\s+REG_SZ\s+(\S+)/);
  return m ? m[1] : null;
}

async function runWindows(getRelease, command) {
  const existing = findWindowsExe();
  if (planAction(command, Boolean(existing)) === "launch") {
    say("launching installed Loomux");
    spawn(existing, [], { detached: true, stdio: "ignore" }).unref();
    return;
  }

  refuseIfRunning();
  const release = await getRelease(existing ? installedWindowsVersion() : null);
  const asset = pickAsset(release, /-setup\.exe$/);
  if (!asset) die(`no Windows installer in release ${release.tag_name}`);

  const dest = path.join(os.tmpdir(), asset.name);
  say(`downloading ${asset.name}`);
  await download(asset.browser_download_url, dest);

  say("installing (silent, per-user)");
  const inst = spawnSync(dest, ["/S"], { stdio: "ignore" });
  fs.rmSync(dest, { force: true });
  if (inst.status !== 0) die("installer exited with an error");

  const exe = findWindowsExe();
  if (exe) {
    say("launching Loomux");
    spawn(exe, [], { detached: true, stdio: "ignore" }).unref();
  } else {
    say(paint(GREEN, "installed — find Loomux in the Start menu"));
  }
}

// ---------- main ----------

async function main() {
  const { command, arg, deprecated } = parseArgs(process.argv.slice(2));
  if (command === "help") {
    process.stdout.write(HELP);
    return;
  }
  if (command === "version") {
    process.stdout.write(`${PKG_VERSION}\n`);
    return;
  }
  if (command === null) {
    die(`unexpected argument "${arg}" — try \`loomux help\``);
  }
  if (deprecated) {
    say(`${deprecated} is a deprecated alias for \`loomux update\` — and it no longer means "install this launcher's own version"; it installs the newest release on your channel, and refuses to go backwards.`);
  }
  if (typeof fetch !== "function") {
    die("Node 18+ is required (global fetch is unavailable in this runtime)");
  }
  // `update` resolves the newest release on the installed build's channel and
  // refuses a downgrade; a plain launch only installs when there is nothing to
  // launch, and resolves the release matching this launcher (so `npx loomux@X`
  // installs app vX). The platform runner supplies the installed version because
  // only it knows how to probe for one. Fetched lazily: a launch that finds an
  // install never touches the network.
  let releasePromise = null;
  const getRelease = (installed) => {
    if (!releasePromise) {
      say("fetching release info");
      releasePromise =
        command === "update" ? resolveUpdateRelease(installed) : resolveRelease();
    }
    return releasePromise;
  };
  switch (process.platform) {
    case "linux":
      return runLinux(getRelease, command);
    case "darwin":
      return runMac(getRelease, command);
    case "win32":
      return runWindows(getRelease, command);
    default:
      die(`unsupported platform: ${process.platform}`);
  }
}

// Only run when invoked as the `loomux` bin — `require`d (by test/launcher.test.ts)
// this file just exposes the pure logic, which is where a wrong answer costs a
// running install.
if (require.main === module) {
  main().catch((e) => die(e && e.message ? e.message : String(e)));
}

module.exports = {
  parseArgs,
  planAction,
  parseVersion,
  compareVersions,
  channelOf,
  newestOnChannel,
  updateVerdict,
  pickCachedAppImage,
  appImageVersion,
  loomuxIsRunning,
};

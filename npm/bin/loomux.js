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
//   loomux update     install/refresh the app — the only path that fetches
//                     when something is already installed or cached
//   loomux version    print this launcher's version
//   loomux help       print usage
//
// Plain `loomux` deliberately never upgrades: the silent installers terminate
// a running Loomux to replace its files, so autoupdate killed live agents
// (#815) and the update decision belongs to the human as `loomux update`.
//
// Dependency-free on purpose: `npx loomux` should have nothing to compile
// and nothing to trust beyond Node's own stdlib (Node >=18 for global fetch).

"use strict";

const os = require("os");
const fs = require("fs");
const path = require("path");
const { spawn, spawnSync } = require("child_process");

const REPO = "willem445/loomux";
const { version: PKG_VERSION } = require("../package.json");

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
  loomux update     Install/refresh the app from the matching GitHub release.
                    Reinstalling over a running app closes it, so quit Loomux
                    first — the launcher refuses while it is running.
  loomux version    Print this launcher's version.
  loomux help       Show this help.
  loomux --help, -h Same as \`loomux help\`.
  loomux --version  Same as \`loomux version\`.

Requires Node 18+.
`;

// Resolve argv to a command. Any input we don't recognize — including trailing
// junk after a valid command — comes back as `{command:null}` so main() dies
// with a hint instead of guessing what the user meant.
function parseArgs(argv) {
  if (argv.length === 0) return { command: "launch" };
  if (argv.length !== 1) return { command: null, arg: argv[0] };
  switch (argv[0]) {
    case "help":
    case "--help":
    case "-h":
      return { command: "help" };
    case "version":
    case "--version":
      return { command: "version" };
    case "update":
    case "--reinstall": // compat alias for pre-#845 scripts and muscle memory
      return { command: "update" };
    default:
      return { command: null, arg: argv[0] };
  }
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
// installs app vX); fall back to whatever the latest release is.
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
  fs.writeFileSync(dest, buf);
}

function cacheDir() {
  const base =
    process.platform === "win32"
      ? process.env.LOCALAPPDATA || os.tmpdir()
      : process.env.XDG_CACHE_HOME || path.join(os.homedir(), ".cache");
  return path.join(base, "loomux");
}

// ---------- Linux AppImage cache ----------

// The newest cached AppImage for this platform wins, preferring the exact
// version this launcher ships. On Linux the cached AppImage IS the install, so
// plain `loomux` launches whatever is there and never downloads — `loomux
// update` is the only path that fetches again (#845). Download recency is a
// fine stand-in for version recency: cache files are only ever created by a
// download, never touched after.
function pickCachedAppImage(dir, suffix, pkgVersion) {
  let entries;
  try {
    entries = fs.readdirSync(dir, { withFileTypes: true });
  } catch {
    return null;
  }
  const re = new RegExp(`^Loomux_.+_${suffix}\\.AppImage$`);
  const hits = entries
    .filter((e) => e.isFile() && re.test(e.name))
    .map((e) => path.join(dir, e.name))
    .sort((a, b) => fs.statSync(b).mtimeMs - fs.statSync(a).mtimeMs);
  if (hits.length === 0) return null;
  return hits.find((p) => path.basename(p).includes(`_${pkgVersion}_`)) || hits[0];
}

// ---------- running-instance guard ----------

// Is a Loomux desktop app running right now? Installing over one is what makes
// the launcher lethal: the silent installer terminates the running process to
// replace its files, taking down the app and anything running inside it.
//
// Unknown (no probe, or the probe failed) is reported as "not running": on both
// platforms the probe ships with the OS, so a failure means something exotic, and
// a launcher that refuses to install on such a machine is a worse bug than one
// that installs. Plain launch never reaches this guard at all (it never
// installs over anything), so it only fires under `loomux update`.
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
  return false; // Linux runs an AppImage in place; nothing is ever replaced.
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

async function runLinux(getRelease, force) {
  const arch = process.arch;
  const suffix = arch === "arm64" ? "aarch64" : arch === "x64" ? "amd64" : null;
  if (!suffix) die(`unsupported Linux architecture: ${arch}`);

  // Plain launch reuses whatever AppImage is cached — never a fresh download —
  // and `update` forces one.
  const cached = pickCachedAppImage(cacheDir(), suffix, PKG_VERSION);
  if (cached && !force) {
    say(`launching ${path.basename(cached)}`);
    const child = spawn(cached, [], { detached: true, stdio: "ignore" });
    child.unref();
    return;
  }

  const release = await getRelease();
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

async function runMac(getRelease, force) {
  const appPath = "/Applications/Loomux.app";
  if (fs.existsSync(appPath) && !force) {
    say("launching installed Loomux.app");
    spawnSync("open", ["-a", "Loomux"], { stdio: "ignore" });
    return;
  }

  refuseIfRunning();
  const release = await getRelease();
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

async function runWindows(getRelease, force) {
  const existing = findWindowsExe();
  if (existing && !force) {
    say("launching installed Loomux");
    spawn(existing, [], { detached: true, stdio: "ignore" }).unref();
    return;
  }

  refuseIfRunning();
  const release = await getRelease();
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
  const { command, arg } = parseArgs(process.argv.slice(2));
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
  if (typeof fetch !== "function") {
    die("Node 18+ is required (global fetch is unavailable in this runtime)");
  }
  // `update` forces the install path; a plain launch only installs when there
  // is nothing to launch. Fetched lazily: a launch that finds an install never
  // touches the network.
  const force = command === "update";
  let releasePromise = null;
  const getRelease = () => {
    if (!releasePromise) {
      say("fetching release info");
      releasePromise = resolveRelease();
    }
    return releasePromise;
  };
  switch (process.platform) {
    case "linux":
      return runLinux(getRelease, force);
    case "darwin":
      return runMac(getRelease, force);
    case "win32":
      return runWindows(getRelease, force);
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

module.exports = { parseArgs, pickCachedAppImage, loomuxIsRunning };

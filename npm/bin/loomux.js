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

const REINSTALL = process.argv.slice(2).includes("--reinstall");

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

// ---------- platform installers ----------

// The installed app can be older than this launcher (updating the npm package
// replaces only the launcher, never the app it installed earlier). Each
// platform reads the version of whatever is actually installed; a mismatch
// triggers a reinstall. `null` (version undetectable) launches as-is so a
// broken probe can't cause a download-on-every-launch loop.
function shouldLaunchExisting(installed) {
  if (installed === null || installed === PKG_VERSION) return true;
  say(`installed app is v${installed}, launcher is v${PKG_VERSION} — upgrading`);
  return false;
}

async function runLinux(getRelease) {
  const arch = process.arch;
  const suffix = arch === "arm64" ? "aarch64" : arch === "x64" ? "amd64" : null;
  if (!suffix) die(`unsupported Linux architecture: ${arch}`);

  // AppImages are cached under their release asset name, so the version is
  // part of the filename — a cache hit is by construction the right version.
  let dest = path.join(cacheDir(), `Loomux_${PKG_VERSION}_${suffix}.AppImage`);
  if (!fs.existsSync(dest) || REINSTALL) {
    const release = await getRelease();
    const asset = pickAsset(release, new RegExp(`_${suffix}\\.AppImage$`));
    if (!asset) die(`no Linux (${arch}) AppImage in release ${release.tag_name}`);
    dest = path.join(cacheDir(), asset.name);
    if (!fs.existsSync(dest) || REINSTALL) {
      say(`downloading ${asset.name}`);
      await download(asset.browser_download_url, dest);
      fs.chmodSync(dest, 0o755);
    }
  }
  say(`launching ${path.basename(dest)}`);
  // Detach so the GUI outlives this short-lived launcher process.
  const child = spawn(dest, [], { detached: true, stdio: "ignore" });
  child.unref();
}

function installedMacVersion() {
  const out = spawnSync(
    "defaults",
    ["read", "/Applications/Loomux.app/Contents/Info", "CFBundleShortVersionString"],
    { encoding: "utf8" }
  );
  if (out.status !== 0 || !out.stdout) return null;
  return out.stdout.trim() || null;
}

async function runMac(getRelease) {
  const appPath = "/Applications/Loomux.app";
  if (fs.existsSync(appPath) && !REINSTALL && shouldLaunchExisting(installedMacVersion())) {
    say("launching installed Loomux.app");
    spawnSync("open", ["-a", "Loomux"], { stdio: "ignore" });
    return;
  }

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

async function runWindows(getRelease) {
  const existing = findWindowsExe();
  if (existing && !REINSTALL && shouldLaunchExisting(installedWindowsVersion())) {
    say("launching installed Loomux");
    spawn(existing, [], { detached: true, stdio: "ignore" }).unref();
    return;
  }

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
  if (typeof fetch !== "function") {
    die("Node 18+ is required (global fetch is unavailable in this runtime)");
  }
  // Fetched lazily: launching an up-to-date install never touches the network.
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
      return runLinux(getRelease);
    case "darwin":
      return runMac(getRelease);
    case "win32":
      return runWindows(getRelease);
    default:
      die(`unsupported platform: ${process.platform}`);
  }
}

main().catch((e) => die(e && e.message ? e.message : String(e)));

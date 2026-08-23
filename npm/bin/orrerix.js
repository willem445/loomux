#!/usr/bin/env node
// Orrerix npm launcher.
//
//   npm install -g orrerix   # then run `orrerix`
//   npx orrerix              # download + launch in one shot
//
// Orrerix itself is a native (Tauri) desktop app, not a JS program — so this
// package ships no binary. Instead it fetches the matching GitHub release
// asset for the host platform, installs/caches it, and launches it. The
// per-platform logic mirrors install.sh / install.ps1 so all three install
// paths behave the same.
//
// The command has three subcommands (#845):
//
//   orrerix           launch the installed app; install it only if missing
//   orrerix update    install/refresh the app from the newest release on the
//                     installed build's channel — the only path that fetches
//                     when something is already installed or cached
//   orrerix version   print this launcher's version
//   orrerix help      print usage
//
// Plain `orrerix` deliberately never upgrades: the silent installers terminate
// a running app to replace its files, so autoupdate killed live agents (#815)
// and the update decision belongs to the human as `orrerix update`.
//
// `orrerix update` is channel-aware and never downgrades (#815/#816/#846) —
// see the "update resolution" section for why both halves are load-bearing.
//
// Dependency-free on purpose: `npx orrerix` should have nothing to compile
// and nothing to trust beyond Node's own stdlib (Node >=18 for global fetch).

"use strict";

const os = require("os");
const fs = require("fs");
const path = require("path");
const { spawn, spawnSync } = require("child_process");

// The repo releases are fetched from. Renaming the GitHub repo is a separate,
// human-coordinated step (#1153), and this slug is deliberately not changed
// ahead of it: GitHub serves permanent redirects for a renamed repo on the
// REST API and on release-asset downloads alike, so the launcher keeps working
// on either side of that button. When the rename happens this line is the
// whole edit.
const REPO = "willem445/loomux";
const { version: PKG_VERSION, name: PKG_NAME } = require("../package.json");

// ---------- brand identity (#1153 phase 5) ----------
//
// Two axes, and conflating them is the trap this block exists to stop.
//
//   PRODUCT_NAMES  Tauri's `productName`. It names everything the BUNDLER
//                  creates, which is everything this launcher has to
//                  recognise on a user's machine: the macOS bundle
//                  (`Orrerix.app`), the Windows install directory, its
//                  Add/Remove key (tauri-bundler's nsis/installer.nsi defines
//                  UNINSTKEY off ${PRODUCTNAME}), and every release asset
//                  filename. #1153 flipped it.
//
//   MAIN_BINARY    Tauri's `mainBinaryName`, which this app does not set. The
//                  config schema is explicit that it then "uses the output
//                  binary from cargo" — the `loomux` crate — so the executable
//                  INSIDE the bundle is `loomux.exe` / `Contents/MacOS/loomux`
//                  and the rebrand did NOT move it. Reading it as though it
//                  followed the product name is how a probe ends up matching
//                  nothing at all (#1294).
//
// The rule from doc/design/rebrand-protocol.md applies here verbatim: emit
// exactly one spelling, accept every spelling on every reading surface, and
// write the accepted set down exactly once. These arrays are that one place —
// index 0 is the emit spelling, and every reader iterates the whole array
// newest-first, so a machine carrying both installs resolves the current one.
//
// Dropping the old spelling would be a silent regression, never a compile
// error: an `orrerix update` that cannot see a pre-rename install reports
// "nothing installed", which is the exact input `updateBaseline` treats as
// safe to order against this launcher's own version — #816's downgrade guard
// disarmed by a rename.
const PRODUCT_NAMES = ["Orrerix", "Loomux"];
const PRODUCT = PRODUCT_NAMES[0];

// The same set as one regex alternation, built FROM the array rather than
// retyped, so no reader can fall out of step with it.
const NAME_ALT = PRODUCT_NAMES.join("|");

// The cargo crate name, which is what Tauri ships as the bundle's executable.
// Not part of the external rebrand, and unchanged by it.
const MAIN_BINARY = "loomux";

// Executable basenames an install can carry, most likely first. MAIN_BINARY is
// what today's bundler writes; the product names follow because a bundle built
// under a config that renamed the binary to the product would carry one of
// those instead, and an install this launcher cannot find is an install it
// cannot protect.
const EXE_NAMES = [MAIN_BINARY, ...PRODUCT_NAMES];

// The command this package installs, and the launcher's own cache directory
// name. That cache is ours alone — nothing but this launcher writes it — so by
// the ownership rule in doc/design/rebrand-filesystem.md it is ours to rename.
// It is renamed but never MOVED: a cached AppImage may be the running process
// and this launcher has no way to know. CLI_NAMES[0] is the only directory
// ever written; the rest are read, so a pre-rename cache stays launchable and
// ages out on its own.
const CLI_NAMES = ["orrerix", "loomux"];
const CLI = CLI_NAMES[0];

const BLUE = "\x1b[1;34m";
const GREEN = "\x1b[1;32m";
const RED = "\x1b[1;31m";
const RESET = "\x1b[0m";
const tty = process.stderr.isTTY;
const paint = (c, s) => (tty ? `${c}${s}${RESET}` : s);

function say(msg) {
  process.stderr.write(`${paint(BLUE, CLI)} ${msg}\n`);
}
function die(msg) {
  process.stderr.write(`${paint(RED, CLI)} ${msg}\n`);
  process.exit(1);
}

// ---------- CLI ----------

const HELP = `${CLI} ${PKG_VERSION} — ${PRODUCT} desktop launcher

Launches the ${PRODUCT} desktop app (installing it first if needed). Run
\`${CLI}\` with no arguments to launch.

USAGE
  ${CLI}            Launch the installed app, or install it if missing. Never
                    updates an existing install.
  ${CLI} update     Install/refresh the app from the newest GitHub release on
                    your channel: a stable install stays on stable, a beta
                    install takes the newest build of either kind. Never
                    downgrades. On Windows and macOS the launcher refuses while
                    the app is running — the installer would close it.
  ${CLI} version    Print this launcher's version.
  ${CLI} help       Show this help.
  ${CLI} --help, -h Same as \`${CLI} help\`.
  ${CLI} --version  Same as \`${CLI} version\`.

Requires Node 18+.
`;

// Resolve argv to a command. Any input we don't recognize — including trailing
// junk after a valid command — comes back as `{command:null}` so main() dies
// with a hint instead of guessing what the user meant.
//
// The desktop app takes no argv of its own (src-tauri/src/main.rs is a bare
// `loomux_lib::run()`, the cargo crate name), so there is nothing to forward and
// every extra token is a mistake worth reporting rather than passing along.
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
//   plain `orrerix` + something installed  -> launch it, never install (#815)
//   plain `orrerix` + nothing installed    -> install (first run)
//   `orrerix update`                       -> install, always (that is the ask)
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
      "User-Agent": `${CLI}-npm-launcher`,
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

// Prefer the release matching this package's version (so `npx orrerix@X`
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

// The entire `orrerix update` decision, pure so it can be pinned by tests:
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

// What `orrerix update` orders against — the version of the app actually
// installed, since that is the thing an installer would overwrite. Three cases,
// and the difference between the last two is the whole guard:
//
//   nothing installed        -> this launcher's version. Nothing can be
//                               downgraded when there is nothing there, and a
//                               first install still needs a channel to pick.
//   version detected         -> that version.
//   installed but unreadable -> null, which the caller turns into a REFUSAL.
//
// That last case used to substitute the launcher's version too, and that
// silently disarmed the guard for anyone the probe could not read: a per-machine
// Windows install (HKLM) under a stale launcher on PATH would be ordered against
// the LAUNCHER's version, so a 1.1.0-beta11 install with a 0.10.0 launcher
// resolved "newest stable" and installed a downgrade — across channels, with no
// message. Unknown is not safe, and it is not a default; it is the exact
// condition the guard exists for, so it stops.
function updateBaseline(hasExisting, detected) {
  if (!hasExisting) return PKG_VERSION;
  if (detected && parseVersion(detected)) return detected;
  return null;
}

async function resolveUpdateRelease(hasExisting, detected) {
  const current = updateBaseline(hasExisting, detected);
  if (current === null) {
    die(
      `refusing to update: ${PRODUCT} is installed but its version can't be read` +
        (detected ? ` ("${detected}" doesn't parse)` : "") +
        `, so there is no way to tell an update from a downgrade. Install the ` +
        `build you want directly from ` +
        `https://github.com/${REPO}/releases and \`${CLI} update\` will work ` +
        `from there.`
    );
  }
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

// `amd64` / `aarch64` — the arch token Tauri puts in a Linux asset name and in
// the cached AppImage's filename. null for an arch we ship no build for.
function linuxSuffix(arch) {
  return arch === "arm64" ? "aarch64" : arch === "x64" ? "amd64" : null;
}

// The release asset to install, as an END-ANCHORED filename suffix.
//
// Deliberately brand-free, and that is the whole reason a rename costs this
// launcher nothing on the network side. Tauri names every bundle
// `<productName>_<version>_<arch>.<ext>`, so #1153 changed the PREFIX of every
// asset filename from the first post-rename release onward. A resolver that
// matched the prefix would need a fallback list, and would silently stop being
// able to install any release published before the flip — which is every
// release a pinned or stable user can currently be asked to install. Matching
// only the suffix is indifferent to the brand in both directions, so there is
// no list to keep in sync and nothing to forget when the next name changes.
//
// The suffixes stay tight enough to exclude the non-installer assets that share
// the family's shape: `Orrerix_1.3.0_x64.pdb.zip` (release.yml's "House style"
// note) ends `.zip`, matching neither `-setup.exe` nor `_x64.dmg`.
function assetPattern(platform, arch) {
  switch (platform) {
    case "linux": {
      const suffix = linuxSuffix(arch);
      return suffix ? new RegExp(`_${suffix}\\.AppImage$`) : null;
    }
    case "darwin":
      return arch === "arm64" ? /_aarch64\.dmg$/ : /_x64\.dmg$/;
    case "win32":
      return /-setup\.exe$/;
    default:
      return null;
  }
}

/** First asset whose name matches `re`, or null. */
function pickAsset(release, re) {
  const assets = release.assets || [];
  return assets.find((a) => re.test(a.name)) || null;
}

async function download(url, dest) {
  const res = await fetch(url, {
    headers: { "User-Agent": `${CLI}-npm-launcher` },
  });
  if (!res.ok || !res.body) die(`download failed (${res.status}): ${url}`);
  fs.mkdirSync(path.dirname(dest), { recursive: true });
  const buf = Buffer.from(await res.arrayBuffer());
  try {
    fs.writeFileSync(dest, buf);
  } catch (e) {
    // Linux has no running-instance probe (appIsRunning is false there), so
    // the kernel is the one that catches "you are overwriting a running
    // AppImage" — as a raw ETXTBSY errno. Translate it into the same advice
    // refuseIfRunning() gives on the other two platforms.
    if (e && e.code === "ETXTBSY") {
      die(
        `${path.basename(dest)} is running — refusing to overwrite it. Quit ` +
          `that ${PRODUCT} window, then run this again.`
      );
    }
    throw e;
  }
}

function cacheBase() {
  return process.platform === "win32"
    ? process.env.LOCALAPPDATA || os.tmpdir()
    : process.env.XDG_CACHE_HOME || path.join(os.homedir(), ".cache");
}

/** The one cache directory this launcher ever WRITES. */
function cacheDir() {
  return path.join(cacheBase(), CLI);
}

// Every cache directory this launcher READS, current spelling first. The
// pre-rename directory is still full of perfectly launchable AppImages, and
// nothing moves or deletes them (see CLI_NAMES): a Linux user who upgrades the
// launcher and runs plain `orrerix` must still get the build they already have
// rather than a surprise download, and `orrerix update` must still be able to
// read its version to order against.
function cacheDirs() {
  const base = cacheBase();
  return CLI_NAMES.map((name) => path.join(base, name));
}

// ---------- Linux AppImage cache ----------

// The newest cached AppImage for this platform wins, on every platform's rule.
// On Linux the cached AppImage IS the install, so plain `orrerix` launches the
// newest cached build and never downloads — `orrerix update` is the only path
// that fetches again (#845). Download recency is a fine stand-in for version
// recency: cache files are only ever created by a download, never touched
// after. Deliberately no "prefer the launcher's own version" bias — update can
// install a build NEWER than the launcher, and plain launch must surface it.
//
// `dirs` is a list because the rename gave the cache a second directory to read
// (cacheDirs). Recency is compared ACROSS them rather than per-directory: the
// question is "what is the newest build on this machine", and answering it
// per-directory would let a stale pre-rename build beat a newer one purely for
// sitting under the older name.
//
// The filename is matched against every accepted product name, since a cached
// AppImage's name is whatever the release it came from was called. On Linux
// that name is also the ONLY version record there is (appImageVersion), so a
// spelling this misses is a build `update` cannot order against.
//
// stat once per file into a pair, then sort: statting inside the comparator ran
// it O(n log n) times, and threw uncaught if a cache file vanished between the
// readdir and the sort.
function pickCachedAppImage(dirs, suffix) {
  const re = new RegExp(`^(?:${NAME_ALT})_.+_${suffix}\\.AppImage$`);
  const hits = [];
  for (const dir of dirs) {
    let entries;
    try {
      entries = fs.readdirSync(dir, { withFileTypes: true });
    } catch {
      continue; // a cache directory that does not exist is simply empty
    }
    for (const e of entries) {
      if (!e.isFile() || !re.test(e.name)) continue;
      const file = path.join(dir, e.name);
      try {
        hits.push({ file, mtime: fs.statSync(file).mtimeMs });
      } catch {
        // Vanished between readdir and stat — not a launchable install.
      }
    }
  }
  hits.sort((a, b) => b.mtime - a.mtime);
  return hits.length ? hits[0].file : null;
}

// The version of a cached AppImage, read off its filename: Linux has no
// installer and no registry, so the asset name is the only version record there
// is. `Orrerix_1.3.0_amd64.AppImage` → "1.3.0", and the pre-rename
// `Loomux_1.1.0-beta11_amd64.AppImage` → "1.1.0-beta11" — a spelling dropped
// here is a cached build `update` silently cannot order against, which lands on
// updateBaseline's null arm and refuses outright.
function appImageVersion(file) {
  if (!file) return null;
  const re = new RegExp(`^(?:${NAME_ALT})_(.+)_(?:amd64|aarch64)\\.AppImage$`);
  const m = re.exec(path.basename(file));
  return m ? m[1] : null;
}

// ---------- running-instance guard ----------

/** Escape a literal string for use inside a RegExp. */
function escapeRe(literal) {
  return literal.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

// The executable names the running-app guard asks the OS about, most likely
// first — `.exe`-suffixed on Windows, bare on macOS, where the process name is
// the bundle's CFBundleExecutable.
//
// This is the EXE axis, not the product axis (#1294), and the difference is the
// whole bug this function exists to close. tauri-bundler writes the bundle's
// binary under `mainBinaryName`, which this app leaves unset, so it is cargo's
// output (MAIN_BINARY) — the process is `loomux` while the bundle around it is
// `Orrerix.app`. Windows never noticed, because `tasklist`'s IMAGENAME filter
// is case-insensitive and the old product spelling differed only in case;
// macOS's `pgrep -x` is not, so the guard matched nothing there and `update`
// was free to `rm -rf` a running /Applications bundle. Post-rename the product
// spelling stops matching on BOTH platforms, since `Orrerix` and `loomux` do
// not differ by case.
//
// Pure and parameterised on the platform so the derivation is pinned by a test
// rather than by this comment.
function processNames(platform) {
  return platform === "win32" ? EXE_NAMES.map((n) => `${n}.exe`) : EXE_NAMES;
}

// Ask the OS whether a process of exactly this name is running. Split out from
// the caller so the name rule above is testable without a live app, the same
// seam `installedWindowsVersion`'s injectable `query` uses.
function osProcessProbe(platform, name) {
  if (platform === "win32") {
    // A filter that matches nothing still exits 0 ("INFO: No tasks…"), so test
    // the output rather than the status.
    const out = spawnSync("tasklist", ["/FI", `IMAGENAME eq ${name}`, "/NH"], {
      encoding: "utf8",
    });
    return new RegExp(escapeRe(name), "i").test(out.stdout || "");
  }
  if (platform === "darwin") {
    const out = spawnSync("pgrep", ["-x", name], { encoding: "utf8" });
    return out.status === 0 && (out.stdout || "").trim() !== "";
  }
  // Linux runs an AppImage in place; nothing is replaced under it, and an
  // overwrite of the running image is caught as ETXTBSY in download().
  return false;
}

// Is the desktop app running right now? Installing over one is what makes the
// launcher lethal: the silent installer terminates the running process to
// replace its files, taking down the app and anything running inside it.
//
// Every accepted name is probed, not just the current one: the app a user has
// open across the rename is the pre-rename build, and that is precisely the
// install `update` is about to overwrite.
//
// Unknown (no probe, or the probe failed) is reported as "not running": on both
// platforms the probe ships with the OS, so a failure means something exotic, and
// a launcher that refuses to install on such a machine is a worse bug than one
// that installs. Plain launch only reaches this guard when it found nothing to
// launch and must install; its real job is protecting `orrerix update` from an
// install-over-running-app.
function appIsRunning(platform = process.platform, probe = osProcessProbe) {
  // Linux runs an AppImage in place; nothing is replaced under it, and an
  // overwrite of the running image is caught as ETXTBSY in download(). That is
  // a platform policy rather than a property of any one probe, so it lives
  // here — an injected probe must not be able to turn it on.
  if (platform !== "win32" && platform !== "darwin") return false;
  return processNames(platform).some((name) => probe(platform, name));
}

// Refuse to install while the app is running — including under `orrerix update`,
// which is an explicit ask to reinstall, not consent to kill a live instance.
// Quitting first is always the user's call to make, never the launcher's.
function refuseIfRunning() {
  if (!appIsRunning()) return;
  die(
    `${PRODUCT_NAMES.join(" or ")} is running — refusing to install over it. ` +
      "The installer would terminate the running app to replace its files, " +
      "closing every window and anything running inside it. Quit it, then run " +
      "this again."
  );
}

// ---------- platform installers ----------

async function runLinux(getRelease, command) {
  const arch = process.arch;
  const suffix = linuxSuffix(arch);
  if (!suffix) die(`unsupported Linux architecture: ${arch}`);

  // Plain launch reuses whatever AppImage is cached — never a fresh download —
  // and `update` forces one.
  const cached = pickCachedAppImage(cacheDirs(), suffix);
  if (planAction(command, Boolean(cached)) === "launch") {
    say(`launching ${path.basename(cached)}`);
    const child = spawn(cached, [], { detached: true, stdio: "ignore" });
    child.unref();
    return;
  }

  const release = await getRelease(Boolean(cached), appImageVersion(cached));
  const asset = pickAsset(release, assetPattern("linux", arch));
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

// The installed application bundle, or null. Product-major: a machine that
// carries both a pre-rename and a post-rename bundle launches the current one.
// Nothing here removes the other — the rename leaves the two installed side by
// side (the NSIS uninstall key and the bundle directory are both named after
// the product), and deleting an app the user still has is not the launcher's
// call.
function findMacApp() {
  for (const name of PRODUCT_NAMES) {
    const appPath = `/Applications/${name}.app`;
    if (fs.existsSync(appPath)) return appPath;
  }
  return null;
}

// The version macOS recorded for an installed bundle.
function installedMacVersion(appPath) {
  if (!appPath) return null;
  const out = spawnSync(
    "defaults",
    ["read", `${appPath}/Contents/Info`, "CFBundleShortVersionString"],
    { encoding: "utf8" }
  );
  if (out.status !== 0 || !out.stdout) return null;
  return out.stdout.trim() || null;
}

async function runMac(getRelease, command) {
  const existingApp = findMacApp();
  if (planAction(command, Boolean(existingApp)) === "launch") {
    say(`launching installed ${path.basename(existingApp)}`);
    spawnSync("open", ["-a", existingApp], { stdio: "ignore" });
    return;
  }

  refuseIfRunning();
  const release = await getRelease(
    Boolean(existingApp),
    installedMacVersion(existingApp)
  );
  const asset = pickAsset(release, assetPattern("darwin", process.arch));
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

  // Install whatever bundle the image actually carries rather than a hardcoded
  // name: it is `Orrerix.app` from the first post-rename release onward and
  // `Loomux.app` on every release before that, and a launcher pinned to one of
  // them cannot install the other. The destination takes the same name, so an
  // install replaces its own predecessor and never touches a differently-named
  // bundle sitting beside it.
  let dest;
  try {
    const bundle = PRODUCT_NAMES.map((n) => `${n}.app`).find((n) =>
      fs.existsSync(path.join(mount, n))
    );
    if (!bundle) die(`no application bundle inside ${asset.name}`);
    dest = path.join("/Applications", bundle);
    spawnSync("rm", ["-rf", dest]);
    const cp = spawnSync("cp", ["-R", path.join(mount, bundle), "/Applications/"]);
    if (cp.status !== 0) die(`could not copy ${bundle} into /Applications`);
  } finally {
    spawnSync("hdiutil", ["detach", mount, "-quiet"]);
    fs.rmSync(dmg, { force: true });
  }
  // Builds are unsigned; clear quarantine so Gatekeeper won't flag it.
  spawnSync("xattr", ["-cr", dest]);
  say(`launching ${path.basename(dest)}`);
  spawnSync("open", ["-a", dest], { stdio: "ignore" });
}

// Where a Tauri NSIS install puts its executable, most likely first.
//
// Two different names, one path (#1294): the DIRECTORY is the product
// (`installer.nsi` sets `$INSTDIR` to `$LOCALAPPDATA\${PRODUCTNAME}` per-user,
// `$PROGRAMFILES\${PRODUCTNAME}` per-machine) and the EXECUTABLE inside it is
// `${MAINBINARYNAME}.exe`, which is cargo's output and did not move with the
// rebrand. `Programs\` is kept as a root because older Tauri installers used
// it and those installs are still out there.
//
// Product-major, not root-major: a machine carrying both a pre-rename and a
// post-rename install must launch the CURRENT one wherever it sits, rather than
// whichever happens to live under the root that got listed first.
//
// `env` is a parameter so the ordering rule is testable without a real machine.
function windowsExeCandidates(env = process.env) {
  const roots = [];
  if (env.LOCALAPPDATA) {
    roots.push(path.join(env.LOCALAPPDATA, "Programs"), env.LOCALAPPDATA);
  }
  if (env.PROGRAMFILES) roots.push(env.PROGRAMFILES);
  const candidates = [];
  for (const product of PRODUCT_NAMES) {
    for (const root of roots) {
      for (const exe of EXE_NAMES) {
        candidates.push(path.join(root, product, `${exe}.exe`));
      }
    }
  }
  return candidates;
}

function findWindowsExe() {
  return windowsExeCandidates().find((p) => fs.existsSync(p)) || null;
}

// Every hive, registry view and product spelling a Tauri/NSIS install can have
// recorded its version under.
//
// The product spelling is in the KEY: `installer.nsi` defines UNINSTKEY as
// `Software\...\Uninstall\${PRODUCTNAME}`, so the rename moved it, and the new
// installer does not read the old one — which is exactly why the two installs
// coexist instead of upgrading. Reading both spellings is what keeps #816's
// downgrade guard armed across the flip, and `newestVersion` below already does
// the right thing with two answers.
//
// HKCU is a per-user install (the Tauri default); HKLM is a per-machine one —
// which is exactly the `%PROGRAMFILES%\<product>` root windowsExeCandidates
// already looks under — and on 64-bit Windows a 32-bit installer writes its
// keys into the WOW6432Node view, which `reg query /reg:32` selects (`/reg:64`
// selects the native one).
//
// Probing HKCU alone left every per-machine install unreadable, and an install
// the guard cannot read is an install it cannot protect: see updateBaseline.
// The probes are cheap and the answer must be complete, so all of them run.
function uninstallKey(product) {
  return `Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\${product}`;
}

function windowsVersionProbes() {
  const probes = [];
  for (const product of PRODUCT_NAMES) {
    const key = uninstallKey(product);
    probes.push([`HKCU\\${key}`, null]);
    probes.push([`HKLM\\${key}`, "/reg:64"]);
    probes.push([`HKLM\\${key}`, "/reg:32"]);
  }
  return probes;
}

/** The DisplayVersion in `reg query` output, or null. */
function parseDisplayVersion(out) {
  const m = String(out || "").match(/DisplayVersion\s+REG_SZ\s+(\S+)/);
  return m ? m[1] : null;
}

// The newest of a list of versions, skipping any that don't parse; null if none
// do. Newest wins on purpose: if ANY install on this machine is newer than the
// release we resolved, installing that release is a downgrade for that install.
// So a stale per-machine leftover can never unblock a downgrade of a newer
// per-user install, or the other way round.
function newestVersion(versions) {
  let best = null;
  for (const v of versions || []) {
    if (!parseVersion(v)) continue;
    if (best === null || compareVersions(v, best) > 0) best = v;
  }
  return best;
}

// A failed query is an absent key, not an error worth surfacing: on 32-bit
// Windows `/reg:64` simply fails, and a machine with only a per-user install has
// no HKLM key at all. Both are normal.
function regQuery(key, view) {
  const args = ["query", key, "/v", "DisplayVersion"];
  if (view) args.push(view);
  const out = spawnSync("reg", args, { encoding: "utf8" });
  return out.status === 0 ? out.stdout || "" : "";
}

// `query` is injectable so the probe coverage and the newest-wins rule are
// testable without a Windows registry.
function installedWindowsVersion(query = regQuery) {
  const found = [];
  for (const [key, view] of windowsVersionProbes()) {
    const v = parseDisplayVersion(query(key, view));
    if (v) found.push(v);
  }
  return newestVersion(found);
}

async function runWindows(getRelease, command) {
  const existing = findWindowsExe();
  if (planAction(command, Boolean(existing)) === "launch") {
    // Name the install we actually found — it may be the pre-rename one.
    say(`launching installed ${path.basename(path.dirname(existing))}`);
    spawn(existing, [], { detached: true, stdio: "ignore" }).unref();
    return;
  }

  refuseIfRunning();
  const release = await getRelease(Boolean(existing), existing ? installedWindowsVersion() : null);
  const asset = pickAsset(release, assetPattern("win32", process.arch));
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
    say(`launching ${PRODUCT}`);
    spawn(exe, [], { detached: true, stdio: "ignore" }).unref();
  } else {
    say(paint(GREEN, `installed — find ${PRODUCT} in the Start menu`));
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
    die(`unexpected argument "${arg}" — try \`${CLI} help\``);
  }
  if (deprecated) {
    say(`${deprecated} is a deprecated alias for \`${CLI} update\` — and it no longer means "install this launcher's own version"; it installs the newest release on your channel, and refuses to go backwards.`);
  }
  if (typeof fetch !== "function") {
    die("Node 18+ is required (global fetch is unavailable in this runtime)");
  }
  // `update` resolves the newest release on the installed build's channel and
  // refuses a downgrade; a plain launch only installs when there is nothing to
  // launch, and resolves the release matching this launcher (so `npx orrerix@X`
  // installs app vX). The platform runner supplies BOTH whether something is
  // installed and what version it reads, because only it knows how to probe —
  // and the two are separate answers on purpose: "nothing there" is safe to
  // order against this launcher, "there but unreadable" is not (updateBaseline).
  // Fetched lazily: a launch that finds an install never touches the network.
  let releasePromise = null;
  const getRelease = (hasExisting, detected) => {
    if (!releasePromise) {
      say("fetching release info");
      releasePromise =
        command === "update"
          ? resolveUpdateRelease(hasExisting, detected)
          : resolveRelease();
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

// Only run when invoked as the `orrerix` bin — `require`d (by
// test/launcher.test.ts) this file just exposes the pure logic, which is where
// a wrong answer costs a running install.
if (require.main === module) {
  main().catch((e) => die(e && e.message ? e.message : String(e)));
}

module.exports = {
  PRODUCT_NAMES,
  CLI_NAMES,
  EXE_NAMES,
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
  windowsExeCandidates,
  windowsVersionProbes,
  pickCachedAppImage,
  appImageVersion,
  assetPattern,
  pickAsset,
  processNames,
  appIsRunning,
};

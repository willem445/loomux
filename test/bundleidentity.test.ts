// One binary name, everywhere (#1562 slice A).
//
// The executable's basename is not configured anywhere: `mainBinaryName` is
// unset in `tauri.conf.json`, so Tauri ships "the output binary from cargo" —
// which means `src-tauri/Cargo.toml`'s `[package] name` IS the name of the
// installed exe, of `target/release/<name>.pdb`, of the WER dump files, of the
// `--webview-exe-name=` argument WebView2 passes its browser process, and of
// `cargo … -p <name>`. Several files spell that name out by hand.
//
// Renaming the package is therefore a many-site edit with nothing mechanical
// holding the sites together, and a HALF-rename is worse than none: CI's E2E
// job would launch a path that does not exist, `symbolicate.yml` would build a
// package cargo no longer has, `release.yml` would zip a missing PDB, and
// `scripts/check-versions.js` would stop finding the lockfile entry it reads
// the release version out of — each failing in a different workflow, at a
// different time, with nothing saying "you renamed one thing".
//
// So: one name, taken from the manifest, asserted at every site that spells it.
// This test is deliberately green in BOTH consistent states (all `loomux`, all
// `orrerix`) — it polices agreement, not a particular spelling, which is what
// makes it survive the rename it was written for.
//
// Two instruments, because named sites and exhaustive shapes fail differently:
//
//   - SITES is the specific half. Each row names the one construct that must
//     carry the binary name, so a stale one fails with a message a reader can
//     act on. It cannot see a site nobody thought to list.
//   - The SHAPE scan is the exhaustive half, and it is default-deny: every
//     `<token>.exe` / `<token>.pdb` in those files must BE the binary name or
//     be on ALLOW with a reason. It decides on the shape of the token, never on
//     the name of the binding around it, so a rename cannot step over it.
//
// See doc/design/rebrand-bundle.md.
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..");
const read = (rel: string) => readFileSync(join(ROOT, rel), "utf8");

/**
 * The source of truth: `[package] name` in `src-tauri/Cargo.toml`.
 *
 * Scoped to the `[package]` section rather than matched file-wide, because
 * `[lib] name = "loomux_lib"` is the very next `name =` line in the file and
 * is deliberately NOT renamed — a file-wide match would read the lib name and
 * this whole test would police the wrong string.
 */
function cargoBinaryName(): string {
  const toml = read("src-tauri/Cargo.toml");
  const start = toml.indexOf("[package]");
  assert.ok(start >= 0, "src-tauri/Cargo.toml must declare a [package] section");
  const rest = toml.slice(start + "[package]".length);
  const end = rest.search(/^\[/m);
  const section = end === -1 ? rest : rest.slice(0, end);
  const m = /^name = "([A-Za-z0-9_-]+)"/m.exec(section);
  assert.ok(m, "src-tauri/Cargo.toml's [package] section must declare a name");
  return m![1];
}

/**
 * The launcher's `LEGACY_MAIN_BINARY` — the ONE place the pre-rename
 * executable name is still spelled on purpose.
 *
 * The launcher has to recognise an install it did not create (#1225's
 * accept-every-spelling rule, and #816's downgrade guard, which is disarmed by
 * an install it cannot see). So `npm/bin/orrerix.js` is the one file where a
 * literal `loomux.exe` is correct after the rename — and rather than hardcode
 * that exemption, the scan reads it from the constant, so the exemption is only
 * ever as wide as what the launcher actually probes.
 */
function legacyBinaryName(): string {
  const m = /^const LEGACY_MAIN_BINARY = "([A-Za-z0-9_-]+)";/m.exec(read("npm/bin/orrerix.js"));
  assert.ok(
    m,
    "npm/bin/orrerix.js must declare LEGACY_MAIN_BINARY — it is what lets `orrerix update` " +
      "still see a pre-rename install, which is what keeps #816's downgrade guard armed"
  );
  return m![1];
}

/**
 * Every construct that spells the binary name out by hand. `re` must capture
 * the basename in group 1, and must be global: several rows legitimately match
 * more than once and ALL of them have to agree.
 */
const SITES: Array<{ file: string; what: string; re: RegExp }> = [
  {
    file: ".github/workflows/ci.yml",
    what: "the E2E job's LOOMUX_E2E_EXE path",
    re: /^\s*LOOMUX_E2E_EXE:.*\\target\\debug\\([A-Za-z0-9_-]+)\.exe\s*$/gm,
  },
  {
    file: ".github/workflows/ci.yml",
    what: "the WebView2 AdditionalBrowserArguments HKLM policy value name (a per-app key, named after the exe)",
    re: /-Name "([A-Za-z0-9_-]+)\.exe"/g,
  },
  {
    file: "e2e/fixtures.ts",
    what: "DEFAULT_EXE, which is what runs when LOOMUX_E2E_EXE is unset",
    re: /^const DEFAULT_EXE = .*"\.\.\/target\/debug\/([A-Za-z0-9_-]+)\.exe"/gm,
  },
  {
    file: "e2e/fixtures.ts",
    what: "the --webview-exe-name= filter that identifies our own WebView2 browser processes",
    re: /--webview-exe-name=([A-Za-z0-9_-]+)\.exe/g,
  },
  {
    file: ".github/workflows/symbolicate.yml",
    what: "the cargo build package selector",
    re: /cargo build[^\n]*\s-p\s+([A-Za-z0-9_-]+)/g,
  },
  {
    file: ".github/workflows/symbolicate.yml",
    what: "the target/release exe and pdb paths",
    re: /target\/release\/([A-Za-z0-9_-]+)\.(?:exe|pdb)/g,
  },
  {
    file: ".github/workflows/symbolicate.yml",
    what: "the symbols artifact name",
    re: /artifact=([A-Za-z0-9_-]+)-symbols-/g,
  },
  {
    file: ".github/workflows/release.yml",
    what: "the PDB source path the release asset is zipped from",
    re: /Compress-Archive -Path target\/release\/([A-Za-z0-9_-]+)\.pdb/g,
  },
  {
    file: "scripts/check-versions.js",
    what: "the exact-match string that finds the package's Cargo.lock entry",
    re: /=== 'name = "([A-Za-z0-9_-]+)"'/g,
  },
  {
    file: "npm/bin/orrerix.js",
    what: "the launcher's MAIN_BINARY",
    re: /^const MAIN_BINARY = "([A-Za-z0-9_-]+)";/gm,
  },
  {
    file: "CLAUDE.md",
    what: "the Commands table's one-backend-test invocation",
    re: /cargo test --locked -p ([A-Za-z0-9_-]+)/g,
  },
];

/** The files the exhaustive shape scan reads. */
const SHAPE_FILES = [
  ".github/workflows/ci.yml",
  ".github/workflows/symbolicate.yml",
  ".github/workflows/release.yml",
  "e2e/fixtures.ts",
  "npm/bin/orrerix.js",
];

/**
 * A `<token>.exe` / `<token>.pdb` occurrence. The trailing guard keeps
 * `re.exec(` from reading as an `re.exe` file.
 */
const SHAPE = /([A-Za-z0-9_.${}-]+)\.(?:exe|pdb)(?![A-Za-z0-9_])/g;

/**
 * Tokens the shape scan sees that are NOT this binary, each with the reason.
 * Default-deny: anything not listed is a finding. A row nothing matches any
 * more is asserted stale below, so the list cannot rot into a blanket
 * exemption. `file`, where present, scopes the row to one file — an exemption
 * that is correct in the launcher is not correct in a workflow.
 */
type AllowRow = { token: string | RegExp; why: string; file?: string };

const ALLOW: AllowRow[] = [
  {
    token: "msedgewebview2",
    why: "WebView2's own browser process — Microsoft's binary, which e2e/fixtures.ts asks the OS about by name",
  },
  {
    token: "setup",
    why: "the NSIS installer asset's `-setup.exe` suffix: tauri-bundler names it off productName, not off the cargo binary",
  },
  {
    token: "-setup",
    why: "the same suffix quoted as a glob (`*-setup.exe`) in prose and in the release-notes template",
  },
  {
    token: /^Orrerix_/,
    why: "release-asset filenames (`Orrerix_<version>_x64-setup.exe`, `..._x64.pdb.zip`) — the productName axis, flipped by #1153 phase 5",
  },
  {
    token: "_x64",
    why: "the tail of that same asset name where the version is interpolated (`Orrerix_$(...)_x64.pdb.zip`), so the prefix is not literal text here",
  },
  {
    token: "steps",
    why: "`steps.pdb.outputs.name` — a GitHub Actions step id, not a filename",
  },
  {
    token: "${MAINBINARYNAME}",
    why: "an NSIS template variable quoted in a comment about installer.nsi's own source",
  },
  {
    token: "${exe}",
    why: "interpolated from the launcher's EXE_NAMES array, which is the single place those names are written",
  },
  {
    token: "${n}",
    why: "the same array, interpolated in processNames()",
  },
];

const allowKey = (row: AllowRow) =>
  `${row.file ?? "*"}|${typeof row.token === "string" ? row.token : String(row.token)}`;

const allowed = (rows: AllowRow[], file: string, token: string) =>
  rows.find(
    (r) =>
      (r.file === undefined || r.file === file) &&
      (typeof r.token === "string" ? r.token === token : r.token.test(token))
  );

function capturesOf(src: string, re: RegExp): string[] {
  return [...src.matchAll(new RegExp(re.source, re.flags))].map((m) => m[1]);
}

type Scan = {
  offenders: string[];
  siteHits: Map<string, number>; // "file: what" -> matches the pattern made
  shapeHits: Map<string, number>; // file -> tokens that ARE the expected name
  allowedHit: Set<string>;
  rows: AllowRow[]; // ALLOW plus the derived legacy-name row
};

/**
 * The whole guard as a pure function of the name it expects, so the test can
 * run it against a name the tree does NOT use and check that it goes red.
 */
function scan(expected: string): Scan {
  const offenders: string[] = [];
  const siteHits = new Map<string, number>();
  const shapeHits = new Map<string, number>();
  const allowedHit = new Set<string>();

  // The legacy-name exemption is derived, not typed: it is exactly the string
  // the launcher's own LEGACY_MAIN_BINARY carries, and only in the launcher.
  const rows: AllowRow[] = [
    ...ALLOW,
    {
      token: legacyBinaryName(),
      file: "npm/bin/orrerix.js",
      why: "LEGACY_MAIN_BINARY — the launcher must keep recognising a pre-rename install, so this file spells the old exe name on purpose (doc/design/rebrand-protocol.md: emit one spelling, accept every spelling)",
    },
  ];

  for (const { file, what, re } of SITES) {
    const key = `${file}: ${what}`;
    const found = capturesOf(read(file), re);
    siteHits.set(key, (siteHits.get(key) ?? 0) + found.length);
    for (const name of found) {
      if (name !== expected) {
        offenders.push(`${file}: ${what} names "${name}", not "${expected}"`);
      }
    }
  }

  for (const file of SHAPE_FILES) {
    const lines = read(file).split(/\r?\n/);
    let mine = 0;
    lines.forEach((line, i) => {
      for (const m of line.matchAll(new RegExp(SHAPE.source, "g"))) {
        const token = m[1];
        if (token === expected) {
          mine += 1;
          continue;
        }
        const row = allowed(rows, file, token);
        if (row) {
          allowedHit.add(allowKey(row));
          continue;
        }
        offenders.push(
          `${file}:${i + 1}: "${token}" is neither the binary name ("${expected}") nor an argued ALLOW row — ${line.trim()}`
        );
      }
    });
    shapeHits.set(file, mine);
  }

  return { offenders, siteHits, shapeHits, allowedHit, rows };
}

test("every surface that spells the executable's name agrees with src-tauri/Cargo.toml", () => {
  const expected = cargoBinaryName();
  const { offenders, siteHits, shapeHits, allowedHit, rows } = scan(expected);

  // The finding first, the controls after. A half-rename trips several of
  // these at once — every stale site AND "this file no longer mentions the new
  // name" — and the list of stale sites is the one a reader can act on. The
  // controls below still run on every green round, which is when they are what
  // makes the green mean anything.
  assert.deepEqual(
    offenders,
    [],
    `the executable's name must be spelled the same everywhere. It comes from cargo (` +
      `mainBinaryName is unset), so src-tauri/Cargo.toml's [package] name decides it and ` +
      `every site below has to follow.\n` +
      offenders.join("\n")
  );

  // Non-vacuity, per site. An absence-only assertion ("no offenders") passes
  // just as well when a pattern matched nothing at all, and a reworded YAML
  // key or a moved constant is exactly how that happens.
  for (const [key, n] of siteHits) {
    assert.ok(
      n > 0,
      `the pattern for ${key} matched nothing — that site is no longer policed, so it ` +
        `could be renamed alone and this test would still pass`
    );
  }

  // Non-vacuity, per scanned file, counted at the VERIFIED site: every file in
  // SHAPE_FILES must actually contain the binary name. A file that stopped
  // mentioning it is a file this scan is watching for nothing.
  for (const [file, n] of shapeHits) {
    assert.ok(
      n > 0,
      `${file} carries no "${expected}.exe"/"${expected}.pdb" occurrence at all — either the ` +
        `binary reference moved out of it (drop the row) or the scan has gone blind to it`
    );
  }

  // A stale ALLOW row is an exemption nobody re-checked.
  for (const row of rows) {
    assert.ok(
      allowedHit.has(allowKey(row)),
      `ALLOW exempts ${row.token}${row.file ? ` in ${row.file}` : ""} (${row.why}) but nothing ` +
        `in the scanned files matches it any more — drop the row rather than leaving an ` +
        `unexamined exemption behind`
    );
  }

  // The legacy name has to BE a different name. If someone "simplified"
  // LEGACY_MAIN_BINARY to the current one, the launcher would probe a single
  // name twice, `orrerix update` would stop seeing a pre-rename install, and
  // #816's downgrade guard would go quietly unarmed — with every test that
  // merely checks "both names are probed" still green.
  assert.notEqual(
    legacyBinaryName(),
    expected,
    "LEGACY_MAIN_BINARY must name the PREVIOUS executable, not the current one"
  );
});

test("the NSIS pre-install hook guards the PREVIOUS binary, and its file is really there", () => {
  // Nothing in CI exercises this. ci.yml builds with `--no-bundle`, so NSIS
  // never runs there and a hooks path that does not resolve would first fail
  // during a release build — the one place a loud failure is expensive. This
  // pin is what makes it fail in `npm test` instead.
  //
  // The path is resolved by tauri-bundler with `dunce::canonicalize`, against
  // the process cwd, which tauri-cli's `setup()` has set to the tauri dir
  // (`set_current_dir(dirs.tauri)`) before the bundler runs — so a relative
  // installerHooks is relative to `src-tauri/`. Both facts read off
  // tauri-cli-v2.11.4, the tag package-lock.json pins.
  const conf = JSON.parse(read("src-tauri/tauri.conf.json"));
  const hooks: string | undefined = conf?.bundle?.windows?.nsis?.installerHooks;
  assert.ok(
    hooks,
    "bundle.windows.nsis.installerHooks must stay wired up — without it the hook file is dead text"
  );
  assert.ok(
    !hooks.startsWith("/") && !/^[A-Za-z]:/.test(hooks),
    `installerHooks must be relative to src-tauri/, not absolute: ${hooks}`
  );

  const src = read(join("src-tauri", hooks)); // throws if the file moved
  const m = /!insertmacro CheckIfAppIsRunning "([A-Za-z0-9_-]+)\.exe"/.exec(src);
  assert.ok(
    m,
    `${hooks} must insert CheckIfAppIsRunning for an executable name — that is the whole hook`
  );

  // The bundler's own installer.nsi already inserts this macro for
  // `${MAINBINARYNAME}.exe`. The hook exists ONLY to cover the name the
  // bundler cannot know about: the previous one. Pointed at the current name
  // it is a duplicate of a check that already runs, and the stranding path it
  // was written for is open again with nothing red to say so.
  assert.equal(
    m![1],
    legacyBinaryName(),
    `the hook must check the PREVIOUS executable (LEGACY_MAIN_BINARY). The current one is ` +
      `already covered by installer.nsi's own CheckIfAppIsRunning "\${MAINBINARYNAME}.exe".`
  );
  assert.notEqual(m![1], cargoBinaryName(), "...which is not the current one");
});

test("the guard discriminates — it reports findings when the name does not match", () => {
  // The control for the instrument itself. Every assertion above is
  // absence-shaped, and an absence is what a scan that examined nothing also
  // produces. Running the identical scan against a name the tree does not use
  // must produce findings from BOTH halves, or "no offenders" was never
  // evidence of anything.
  const bogus = scan("definitely-not-the-binary-name");

  assert.ok(
    bogus.offenders.length > 0,
    "scanning for a name nothing uses produced no findings — the scan is inert"
  );

  // `<file>:<line>:` prefixes come from the shape half; the site half has no
  // line number. Both must fire, or one instrument is dead while the other
  // carries the test.
  const fromShape = bogus.offenders.filter((o) => /^\S+:\d+: /.test(o));
  const fromSites = bogus.offenders.filter((o) => !/^\S+:\d+: /.test(o));
  assert.ok(fromSites.length > 0, "the named-site half produced nothing — SITES is inert");
  assert.ok(fromShape.length > 0, "the shape half produced nothing — the SHAPE scan is inert");

  // And it is not a scan that flags everything: the real name is the one that
  // comes back clean, which is what the first test asserts in detail.
  assert.equal(
    scan(cargoBinaryName()).offenders.length,
    0,
    "the real binary name must scan clean — see the first test for the offender list"
  );
});

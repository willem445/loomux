// One binary name and one bundle identifier, everywhere (#1562 slices A and B).
//
// Two axes, two halves of this file. The first is the executable's name; the
// second, at the bottom, is the Tauri bundle identifier. They are independent
// values that fail the same way — several files spell each one out by hand,
// and a HALF-rename of either is worse than no rename — so they are policed by
// the same two instruments, and the identifier half is written as a deliberate
// mirror of the binary half rather than as something new to learn.
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
//     the name of the binding around it, so a rename cannot step over it. Its
//     regex is cross-checked against a raw count of `.exe`/`.pdb` in the same
//     file, so a pattern that cannot see one of its own subjects fails as a
//     blind instrument rather than passing as "no offenders".
//
// What the SHAPE half does NOT see, stated rather than left to be discovered:
// the binary's name where it appears WITHOUT its extension — `Contents/MacOS/
// <name>`, `/usr/bin/<name>`, `-p <name>`, `cargo … -p <name>`. There is no
// shape to key on there; a bare word is just a word. Those sites are covered by
// SITES rows instead, which means they are covered only where someone listed
// them: a NEW file that names the binary bare is invisible to this test. The
// same is true of a name assembled at runtime (`"orrer" + "ix.exe"`). Neither
// exists today.
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
 * The same subjects counted WITHOUT the name pattern — the instrument's own
 * cross-check. `SHAPE`'s character class is a guess about the alphabet an
 * executable name may be spelled with, and a guess that stops one character
 * short silently drops a subject and reports a clean scan.
 */
const RAW_SHAPE = /\.(?:exe|pdb)(?![A-Za-z0-9_])/g;

/**
 * ...and the reconciliation: a raw hit SHAPE did not match is only acceptable
 * when the character in front of the dot means there is no name in front of it.
 * Default-deny on that character, so an alphabet gap fails loudly instead of
 * being absorbed as "just prose".
 */
const NAMELESS_BEFORE: Array<{ ch: string; why: string }> = [
  { ch: " ", why: "prose naming a bare extension: `the Windows .pdb.zip`" },
  { ch: "`", why: "a backticked bare extension in a comment: `` `.exe`-suffixed ``" },
  {
    ch: "\\",
    why:
      "a JS regex literal escaping the dot (`/-setup\\.exe$/`). NOTE the residual this " +
      "names: SHAPE cannot see a name behind an escaped dot, so a regex spelled " +
      "`/orrerix\\.exe$/` would be invisible to the shape half. No such regex exists today; " +
      "if one is added it needs a SITES row.",
  },
];

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
  seenShapes: Map<string, number>; // file -> tokens the SHAPE pattern matched at all
  rawShapes: Map<string, number>; // file -> raw `.exe`/`.pdb` occurrences
  unexplained: string[]; // raw hits SHAPE missed whose preceding char is not on NAMELESS_BEFORE
  namelessHit: Set<string>;
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
  const seenShapes = new Map<string, number>();
  const rawShapes = new Map<string, number>();
  const unexplained: string[] = [];
  const namelessHit = new Set<string>();
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
    const src = read(file);
    const lines = src.split(/\r?\n/);
    let raw = 0;
    for (const m of src.matchAll(new RegExp(RAW_SHAPE.source, "g"))) {
      raw += 1;
      const before = m.index! > 0 ? src[m.index! - 1] : "";
      if (/[A-Za-z0-9_.${}-]/.test(before)) continue; // SHAPE will have matched it
      const row = NAMELESS_BEFORE.find((r) => r.ch === before);
      if (row) {
        namelessHit.add(row.ch);
        raw -= 1; // deliberately nameless: nothing here for SHAPE to check
        continue;
      }
      unexplained.push(
        `${file}: a ".exe"/".pdb" preceded by ${JSON.stringify(before)}, which is neither a ` +
          `character SHAPE can read as part of a name nor an argued NAMELESS_BEFORE row`
      );
    }
    rawShapes.set(file, raw);
    let mine = 0;
    let seen = 0;
    lines.forEach((line, i) => {
      for (const m of line.matchAll(new RegExp(SHAPE.source, "g"))) {
        const token = m[1];
        seen += 1;
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
    seenShapes.set(file, seen);
  }

  return { offenders, siteHits, shapeHits, seenShapes, rawShapes, unexplained, namelessHit, allowedHit, rows };
}

test("every surface that spells the executable's name agrees with src-tauri/Cargo.toml", () => {
  const expected = cargoBinaryName();
  const {
    offenders,
    siteHits,
    shapeHits,
    seenShapes,
    rawShapes,
    unexplained,
    namelessHit,
    allowedHit,
    rows,
  } = scan(expected);

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

  // The instrument, checked against a raw count of its own container. Every
  // literal `.exe`/`.pdb` in a scanned file must have produced a SHAPE match,
  // or be one the reconciliation below explains away; the ones the pattern
  // cannot see are unguarded — they could be renamed alone and this test would
  // still report a clean scan.
  assert.deepEqual(
    unexplained,
    [],
    `a ".exe"/".pdb" occurrence sits behind a character SHAPE's class does not cover, and ` +
      `nothing on NAMELESS_BEFORE explains it. Either widen the class (if a name can really ` +
      `be spelled that way) or add an argued NAMELESS_BEFORE row.\n` + unexplained.join("\n")
  );
  for (const { ch, why } of NAMELESS_BEFORE) {
    assert.ok(
      namelessHit.has(ch),
      `NAMELESS_BEFORE carries ${JSON.stringify(ch)} (${why}) but nothing in the scanned ` +
        `files sits behind it any more — drop the row rather than leaving an unexamined ` +
        `exemption behind`
    );
  }
  for (const file of SHAPE_FILES) {
    assert.equal(
      seenShapes.get(file) ?? 0,
      rawShapes.get(file) ?? 0,
      `in ${file} the SHAPE pattern matched ${seenShapes.get(file) ?? 0} of the ` +
        `${rawShapes.get(file) ?? 0} literal ".exe"/".pdb" occurrences. Its character class ` +
        `is a guess about the alphabet an executable name may be spelled with, and it just ` +
        `came up short on one of its own subjects.`
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
// ---------------------------------------------------------------------------
// The bundle identifier (#1562 slice B)
// ---------------------------------------------------------------------------
//
// `tauri.conf.json`'s `identifier` is the app's identity to the OS. On Windows
// and Linux, Tauri resolves the webview's user-data folder from it —
// `<data_local_dir>/<identifier>`, holding the WebView2 `localStorage` a user's
// recent repos, default agent and editor command live in — and on macOS it is
// the `CFBundleIdentifier` the microphone (TCC) grant is keyed on.
//
// It is spelled in four places that must agree, in three languages:
//
//   - `src-tauri/tauri.conf.json` — what the shipped build actually IS;
//   - `crates/loomux-engine/src/brand.rs` — `BUNDLE_ID`, what the Rust that
//     performs the one-time profile move believes it is. A disagreement here is
//     silent by construction: `init_webview_profile_from` no-ops for any
//     identifier that is not `BUNDLE_ID`, so the move simply never happens and
//     every existing user's preferences are quietly reset instead;
//   - `src-tauri/tauri.e2e.conf.json` — the E2E build's DIFFERENT identifier,
//     which is the whole of the isolation argument in doc/design/e2e-testing.md
//     (#394: WebView2 keys its shared browser process off the identifier);
//   - `e2e/fixtures.ts` — `EXPECTED_IDENTIFIER`, which the harness verifies the
//     spawned build's WebView2 child is really running under before it drives
//     anything. Stale, and the harness refuses a build that is fine, or accepts
//     one that is not.
//
// Same two instruments as the binary half: named sites (specific, actionable,
// blind to a site nobody listed) plus a default-deny shape scan over every
// `dev.<x>.<y>` token in those files, cross-checked against a raw count of
// `dev.` in the same file so a pattern that cannot see one of its own subjects
// fails as a blind instrument rather than passing as "no offenders".
//
// What the identifier half does NOT see, stated rather than left to be found:
// the identifier where it appears WITHOUT its `dev.` prefix, and the profile
// directory named by a variable (`<data_local_dir>/<identifier>`) rather than
// spelled. Neither exists today.
//
// One more, because it is live rather than hypothetical: `ID_SHAPE` reads two
// dot-separated segments and stops, so a SUPERSTRING of an identifier —
// `dev.orrerix.app.other`, a deliberate near-miss specimen in `obs.rs`'s
// `a_non_production_identifier_never_moves_the_profile` — matches as
// `dev.orrerix.app` and is counted as the product identifier. Harmless in that
// direction (it is not reported as an offender when it should not be), and the
// raw cross-check below stays consistent because both patterns see it once. It
// would matter if someone introduced a real third-segment identifier, which
// nothing does. It also cannot check what Tauri does with the
// value — that the shipped build's WebView2 child really runs under it is what
// the `e2e-windows` job proves, and nothing here substitutes for reading it.

/** A `pub const <NAME>: &str = "…";` out of brand.rs. */
function brandConst(name: string): string {
  const src = read("crates/loomux-engine/src/brand.rs");
  const m = new RegExp(`^pub const ${name}: &str = "([A-Za-z0-9_.-]+)";`, "m").exec(src);
  assert.ok(m, `crates/loomux-engine/src/brand.rs must declare ${name}`);
  return m![1];
}

/** The `identifier` field of a Tauri config. */
function configIdentifier(rel: string): string {
  const id = JSON.parse(read(rel)).identifier;
  assert.equal(typeof id, "string", `${rel} must carry a string identifier`);
  return id as string;
}

/**
 * Named sites, per identifier. `re` captures the identifier in group 1 and
 * must be global — several rows match more than once and all of them agree or
 * none of them do.
 */
type IdSite = { file: string; what: string; re: RegExp };

const PROD_SITES: IdSite[] = [
  {
    file: "crates/loomux-engine/src/brand.rs",
    what: "brand::BUNDLE_ID, which decides whether the profile move runs at all",
    re: /^pub const BUNDLE_ID: &str = "([A-Za-z0-9_.-]+)";/gm,
  },
  {
    file: "e2e/fixtures.ts",
    what: "the refusal message that names the production identifier a stray WebView2 process would belong to",
    re: /production-identifier build \(([A-Za-z0-9_.-]+)\)/g,
  },
];

const E2E_SITES: IdSite[] = [
  {
    file: "e2e/fixtures.ts",
    what: "EXPECTED_IDENTIFIER, which the harness verifies the spawned build's WebView2 child against",
    re: /^const EXPECTED_IDENTIFIER = "([A-Za-z0-9_.-]+)";/gm,
  },
  {
    file: "e2e/fixtures.ts",
    what: "the isolation comment naming the identifier override",
    re: /`identifier` override \(`([A-Za-z0-9_.-]+)`/g,
  },
  {
    file: ".github/workflows/ci.yml",
    what: "the e2e-windows job's comment naming the isolated profile's identifier",
    re: /`tauri\.e2e\.conf\.json`'s `([A-Za-z0-9_.-]+)` identifier/g,
  },
];

/** The files the identifier shape scan reads. */
const ID_SHAPE_FILES = [
  "src-tauri/tauri.conf.json",
  "src-tauri/tauri.e2e.conf.json",
  "e2e/fixtures.ts",
  ".github/workflows/ci.yml",
  "crates/loomux-engine/src/brand.rs",
  // The file that PERFORMS the move, and the one whose disagreement with
  // tauri.conf.json this guard's header calls silent by construction. It was
  // missing from this list in the first cut, which left the identifier spelled
  // in six places there — five doc comments and one test specimen — that
  // nothing read (rev-lead round 1, N2).
  "crates/loomux-engine/src/obs.rs",
];

/** A reverse-DNS identifier token, and the same subjects counted without the name. */
const ID_SHAPE = /dev\.[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+/g;
const RAW_ID_SHAPE = /dev\./g;

type IdScan = {
  offenders: string[];
  siteHits: Map<string, number>;
  shapeHits: Map<string, number>;
  seenShapes: Map<string, number>;
  rawShapes: Map<string, number>;
  allowedHit: Set<string>;
  rows: Array<{ token: string; file: string; why: string }>;
};

/**
 * The whole identifier guard as a pure function of the two identifiers it
 * expects, so the test can run it against values the tree does NOT use and
 * check that it goes red.
 */
function scanIdentifiers(prod: string, e2e: string): IdScan {
  const offenders: string[] = [];
  const siteHits = new Map<string, number>();
  const shapeHits = new Map<string, number>();
  const seenShapes = new Map<string, number>();
  const rawShapes = new Map<string, number>();
  const allowedHit = new Set<string>();

  // The one exemption, derived rather than typed: brand.rs is where the
  // pre-#1562 identifier is spelled on purpose, because the move has to name
  // the directory it moves FROM. Taken from LEGACY_BUNDLE_ID itself, so the
  // exemption is only ever as wide as the constant the move actually reads.
  const rows: Array<{ token: string; file: string; why: string; commentOnly?: boolean }> = [
    {
      token: brandConst("LEGACY_BUNDLE_ID"),
      file: "crates/loomux-engine/src/brand.rs",
      why: "LEGACY_BUNDLE_ID — the source directory of the one-time webview-profile move, and the one place the old identifier is spelled as a VALUE (see that module's own rule)",
    },
    {
      token: brandConst("LEGACY_BUNDLE_ID"),
      file: "crates/loomux-engine/src/obs.rs",
      commentOnly: true,
      why:
        "doc comments explaining which directory the move reads FROM. Scoped to comment " +
        "lines on purpose: the code in that file reaches the old identifier through " +
        "brand::LEGACY_BUNDLE_ID, never as a literal, so a literal on a CODE line there is " +
        "still a finding — which is the whole difference between prose about the move and " +
        "a second place the name is spelled",
    },
  ];

  for (const [sites, expected] of [
    [PROD_SITES, prod],
    [E2E_SITES, e2e],
  ] as Array<[IdSite[], string]>) {
    for (const { file, what, re } of sites) {
      const key = `${file}: ${what}`;
      const found = capturesOf(read(file), re);
      siteHits.set(key, (siteHits.get(key) ?? 0) + found.length);
      for (const name of found) {
        if (name !== expected) {
          offenders.push(`${file}: ${what} names "${name}", not "${expected}"`);
        }
      }
    }
  }

  for (const file of ID_SHAPE_FILES) {
    const src = read(file);
    const lines = src.split(/\r?\n/);
    rawShapes.set(file, [...src.matchAll(new RegExp(RAW_ID_SHAPE.source, "g"))].length);
    let mine = 0;
    let seen = 0;
    lines.forEach((line, i) => {
      for (const m of line.matchAll(new RegExp(ID_SHAPE.source, "g"))) {
        const token = m[0];
        seen += 1;
        if (token === prod || token === e2e) {
          mine += 1;
          continue;
        }
        const isComment = /^\s*(\/\/|#|\*)/.test(line);
        const row = rows.find(
          (r) => r.token === token && r.file === file && (!r.commentOnly || isComment)
        );
        if (row) {
          allowedHit.add(`${row.file}|${row.token}`);
          continue;
        }
        offenders.push(
          `${file}:${i + 1}: "${token}" is neither the product identifier ("${prod}") nor the ` +
            `E2E one ("${e2e}") nor an argued exemption — ${line.trim()}`
        );
      }
    });
    shapeHits.set(file, mine);
    seenShapes.set(file, seen);
  }

  return { offenders, siteHits, shapeHits, seenShapes, rawShapes, allowedHit, rows };
}

test("every surface that spells the bundle identifier agrees with src-tauri/tauri.conf.json", () => {
  const prod = configIdentifier("src-tauri/tauri.conf.json");
  const e2e = configIdentifier("src-tauri/tauri.e2e.conf.json");
  const { offenders, siteHits, shapeHits, seenShapes, rawShapes, allowedHit, rows } =
    scanIdentifiers(prod, e2e);

  assert.deepEqual(
    offenders,
    [],
    `the bundle identifier must be spelled the same everywhere. tauri.conf.json decides it ` +
      `(it is what the shipped build IS) and every site below has to follow.\n` +
      offenders.join("\n")
  );

  // The E2E identifier has to BE a different identifier. If the two ever
  // converged, an E2E run would share the production build's WebView2 browser
  // process (#394) — and `verifyIsolatedBuild`, which checks the child's
  // `--user-data-dir` against EXPECTED_IDENTIFIER, would pass while doing so,
  // because it would be checking against the value it now also matches.
  assert.notEqual(
    e2e,
    prod,
    "tauri.e2e.conf.json must override the identifier to something the product does not use — " +
      "that override IS the E2E isolation (doc/design/e2e-testing.md)"
  );

  // The legacy identifier has to be a third value. If someone "simplified"
  // LEGACY_BUNDLE_ID to the current one, the one-time move would rename a
  // directory onto itself — a no-op that reports success on every arm, with
  // every existing user's preferences silently reset.
  assert.notEqual(
    brandConst("LEGACY_BUNDLE_ID"),
    prod,
    "LEGACY_BUNDLE_ID must name the PREVIOUS identifier, not the current one"
  );

  // Non-vacuity, per site.
  for (const [key, n] of siteHits) {
    assert.ok(
      n > 0,
      `the pattern for ${key} matched nothing — that site is no longer policed, so it could ` +
        `drift alone and this test would still pass`
    );
  }

  // Non-vacuity, per scanned file, counted at the VERIFIED site.
  for (const [file, n] of shapeHits) {
    assert.ok(
      n > 0,
      `${file} carries neither "${prod}" nor "${e2e}" any more — either the identifier moved ` +
        `out of it (drop the row) or the scan has gone blind to it`
    );
  }

  // The instrument against a raw count of its own container.
  for (const file of ID_SHAPE_FILES) {
    assert.equal(
      seenShapes.get(file) ?? 0,
      rawShapes.get(file) ?? 0,
      `in ${file} the identifier pattern matched ${seenShapes.get(file) ?? 0} of the ` +
        `${rawShapes.get(file) ?? 0} literal "dev." occurrences — its shape is a guess about ` +
        `how an identifier may be spelled, and it just came up short on one of its own subjects`
    );
  }

  // A stale exemption is one nobody re-checked.
  for (const row of rows) {
    assert.ok(
      allowedHit.has(`${row.file}|${row.token}`),
      `${row.file} is exempted for "${row.token}" (${row.why}) but nothing in it matches that ` +
        `any more — drop the row rather than leaving an unexamined exemption behind`
    );
  }
});

test("the identifier guard discriminates — it reports findings when the identifiers do not match", () => {
  const prod = configIdentifier("src-tauri/tauri.conf.json");
  const e2e = configIdentifier("src-tauri/tauri.e2e.conf.json");

  // Every assertion above is absence-shaped, and an absence is what a scan that
  // examined nothing also produces. Both halves must report findings against
  // identifiers the tree does not use.
  const bogus = scanIdentifiers("dev.nothing.app", "dev.nothing.e2e");
  assert.ok(bogus.offenders.length > 0, "scanning for identifiers nothing uses produced no findings");
  const fromShape = bogus.offenders.filter((o) => /^\S+:\d+: /.test(o));
  const fromSites = bogus.offenders.filter((o) => !/^\S+:\d+: /.test(o));
  assert.ok(fromSites.length > 0, "the named-site half produced nothing — the identifier SITES are inert");
  assert.ok(fromShape.length > 0, "the shape half produced nothing — the identifier shape scan is inert");

  // Half a rename is the actual failure mode, and it must be caught too: the
  // product config flipped while brand.rs and the harness still say the old
  // thing is what a reader would see as "the identifier changed and nothing
  // went red".
  assert.ok(
    scanIdentifiers("dev.orrerix.next", e2e).offenders.length > 0,
    "flipping ONLY the product identifier must be reported — that is the half-rename this guard exists for"
  );
  assert.ok(
    scanIdentifiers(prod, "dev.orrerix.next").offenders.length > 0,
    "flipping ONLY the E2E identifier must be reported"
  );

  // And it is not a scan that flags everything.
  assert.equal(
    scanIdentifiers(prod, e2e).offenders.length,
    0,
    "the real identifiers must scan clean — see the first identifier test for the offender list"
  );
});

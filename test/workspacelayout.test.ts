// Repo-file pin on the Cargo workspace layout (#888 slice A1 / #847 Phase 0) —
// same species as `releasepromote.test.ts`'s pin on release.yml: read the real
// files off disk and assert the invariants whose violation is SILENT.
//
// The workspace conversion moved three things at once — the lockfile, the
// build-output directory, and the release profile — and each of them fails
// quietly rather than loudly when it drifts back:
//
//   - `[profile.release]` in a NON-root member is ignored by cargo with a
//     warning, not an error. A release build keeps succeeding while dropping
//     lto/codegen-units and the debug settings that put loomux's own function
//     names in a crash backtrace (#53).
//   - A leftover `src-tauri/Cargo.lock` is a file cargo never updates again,
//     but which still parses — and `scripts/check-versions.js` would happily
//     read a version out of it forever.
//   - The E2E exe path is consumed in two places (ci.yml's LOOMUX_E2E_EXE and
//     e2e/fixtures.ts's DEFAULT_EXE fallback). Drift shows up as "exe not
//     found" in a continue-on-error job, long after the edit that caused it.
//
// None of these is reachable from a unit test of product code, and agents are
// banned from running cargo locally (#488), so the shape gets pinned here where
// `npm test` — the frontend suite CI already gates on — can see it.
import { test } from "node:test";
import assert from "node:assert/strict";
import { existsSync, readFileSync } from "node:fs";

function repoFile(rel: string): string {
  return readFileSync(new URL(`../${rel}`, import.meta.url), "utf8");
}

function repoHas(rel: string): boolean {
  return existsSync(new URL(`../${rel}`, import.meta.url));
}

test("the workspace root manifest declares every member and resolver 2", () => {
  assert.ok(repoHas("Cargo.toml"), "the Cargo workspace root manifest must exist at the repo root");
  const root = repoFile("Cargo.toml");

  assert.match(root, /^\[workspace\]$/m, "the root manifest must be a workspace manifest");
  // Explicit, because a VIRTUAL manifest does not inherit the resolver implied
  // by a member's edition — cargo warns and silently falls back to resolver 1,
  // which unifies features across normal and build dependencies differently.
  // That is the ground the getrandom audit in src-tauri/Cargo.toml stands on.
  assert.match(root, /^resolver = "2"$/m, "the workspace must state resolver 2 explicitly");

  // `crates/loomux-server` joined in #888 slice C1a. A member that is declared
  // but absent, or present but undeclared, fails the same silent way the three
  // hazards at the top of this file do: `cargo check --workspace` simply never
  // compiles a directory nobody listed.
  for (const member of ["src-tauri", "crates/loomux-engine", "crates/loomux-server"]) {
    assert.ok(
      new RegExp(`"${member}"`).test(root),
      `the workspace members must include ${member}`
    );
    assert.ok(
      repoHas(`${member}/Cargo.toml`),
      `${member} is declared a workspace member, so ${member}/Cargo.toml must exist`
    );
  }
});

test("[profile.release] lives at the workspace root, where cargo actually reads it", () => {
  const root = repoFile("Cargo.toml");
  const member = repoFile("src-tauri/Cargo.toml");

  assert.match(root, /^\[profile\.release\]$/m, "the root manifest must own [profile.release]");
  for (const setting of [/^lto = true$/m, /^codegen-units = 1$/m, /^debug = "line-tables-only"$/m, /^strip = "debuginfo"$/m]) {
    assert.match(root, setting, `the root [profile.release] must keep ${setting} — see #53 for what debug/strip buy`);
  }

  // The whole point: cargo IGNORES a profile in a non-root member and only
  // warns. Moving these back would be a silent downgrade of every release
  // binary, with green CI.
  assert.doesNotMatch(
    member,
    /^\[profile\./m,
    "src-tauri/Cargo.toml must not declare any [profile.*] — cargo ignores profiles in a non-root workspace member (warning only), so the settings would silently stop applying"
  );
});

test("there is exactly one Cargo.lock, at the workspace root", () => {
  assert.ok(repoHas("Cargo.lock"), "the workspace lockfile must be at the repo root");
  assert.ok(
    !repoHas("src-tauri/Cargo.lock"),
    "src-tauri/Cargo.lock must be gone — cargo keeps one lock per workspace, and a leftover second one is a file nothing updates but everything still parses"
  );
});

// Behavioural, not textual: re-implements (does not call) the exact-equality
// rule `scripts/check-versions.js`'s cargoLockVersion() uses, against the
// real lockfile, and proves it resolves the app's version rather than the
// engine crate's permanent 0.0.0.
function lockVersionOf(lock: string, name: string): string | undefined {
  const lines = lock.split(/\r?\n/);
  for (let i = 0; i < lines.length; i++) {
    if (lines[i].trim() === `name = "${name}"`) {
      const m = lines[i + 1] && lines[i + 1].match(/^version\s*=\s*"([^"]+)"/);
      if (m) return m[1];
    }
  }
  return undefined;
}

/** Escape a literal for use inside a RegExp — the package name reaches one. */
function escapeRe(literal: string): string {
  return literal.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

/** The app package's name, from `src-tauri/Cargo.toml`'s `[package]` section. */
function appPackageName(): string {
  const toml = repoFile("src-tauri/Cargo.toml");
  const start = toml.indexOf("[package]");
  const rest = toml.slice(start + "[package]".length);
  const end = rest.search(/^\[/m);
  const m = /^name = "([A-Za-z0-9_-]+)"/m.exec(end === -1 ? rest : rest.slice(0, end));
  assert.ok(m, "src-tauri/Cargo.toml's [package] section must declare a name");
  return m![1];
}

test("the lockfile's app entry is not shadowed by its sibling members", () => {
  const lock = repoFile("Cargo.lock");
  const appVersion = JSON.parse(repoFile("package.json")).version;
  const app = appPackageName();

  // Read from the manifest, not typed in: the package was renamed `loomux` →
  // `orrerix` in #1562, and a hardcoded name here would have gone on asserting
  // something true about a lock entry that no longer exists.
  assert.equal(
    lockVersionOf(lock, app),
    appVersion,
    `the \`${app}\` entry in the root Cargo.lock must carry the release version — this is the field scripts/check-versions.js reads`
  );
  // The siblings stay at a version the app's can never be, so a loose name
  // match in the checker fails loudly instead of silently agreeing.
  //
  // Note what this pair does NOT witness any more. It was written when the app
  // package was `loomux`, a strict prefix of both siblings — the collision that
  // made exact-equality load-bearing. `orrerix` is a prefix of neither, so the
  // real lockfile no longer contains the shape this was guarding. Rather than
  // relax the claim to fit today's names, the prefix case moves to its own test
  // below, on a fixture that still has it (#689: relocate the property onto a
  // witness that still distinguishes).
  for (const sibling of ["loomux-engine", "loomux-server"]) {
    assert.equal(
      lockVersionOf(lock, sibling),
      "0.0.0",
      `${sibling} is publish=false and deliberately pinned at 0.0.0, outside the release version set`
    );
    assert.notEqual(
      lockVersionOf(lock, sibling),
      appVersion,
      "the lock entries must stay distinguishable, so a loose name match in check-versions.js fails loudly instead of silently agreeing"
    );
  }
});

test("the lockfile lookup resolves by exact name, not by prefix", () => {
  // The property the test above used to carry, on a fixture that still
  // exhibits it. Cargo.lock is alphabetical, so a prefix sibling can sit either
  // side of the entry being looked for — both orders are here, because a
  // first-match-wins bug is invisible from whichever side happens to come
  // second today.
  //
  // This is what `scripts/check-versions.js`'s `=== 'name = "<app>"'` buys, and
  // it is the assertion that fails if anyone ever relaxes it to `startsWith` or
  // `includes` on the grounds that "nothing collides anymore".
  const synthetic = [
    "[[package]]",
    'name = "app-core"',
    'version = "0.0.0"',
    "",
    "[[package]]",
    'name = "app"',
    'version = "9.9.9"',
    "",
    "[[package]]",
    'name = "app-server"',
    'version = "0.0.0"',
    "",
  ].join("\n");

  assert.equal(
    lockVersionOf(synthetic, "app"),
    "9.9.9",
    "a lookup for `app` must not resolve to `app-core` (which sorts before it) or `app-server` (after)"
  );
  assert.equal(lockVersionOf(synthetic, "app-core"), "0.0.0");
  assert.equal(lockVersionOf(synthetic, "app-server"), "0.0.0");
  assert.equal(
    lockVersionOf(synthetic, "ap"),
    undefined,
    "a name that is merely a prefix of a real entry must resolve to nothing at all"
  );
});

test("check-versions.js reads the workspace-root lockfile", () => {
  const script = repoFile("scripts/check-versions.js");
  assert.match(
    script,
    /cargoLockVersion\('Cargo\.lock'\)/,
    "check-versions.js must read the root Cargo.lock — the src-tauri one no longer exists"
  );
});

test("the non-desktop crates are publish=false and declare no tauri dependency", () => {
  // The rule is the same for both and the reason is the same for both: a core —
  // or a daemon — that needs webkit2gtk in order to build is not a deployment
  // shape. The arrow runs `loomux-server -> loomux-engine -> (no tauri)`, and
  // `src-tauri -> loomux-engine`; it never points back.
  //
  // The manifest-level half of the rule. Plan-463 slice A4 adds the CI step
  // that denies tauri from the resolved `cargo tree`; this catches the
  // straightforward way it would get there in the meantime, and costs nothing.
  for (const crate of ["loomux-engine", "loomux-server"]) {
    const manifest = repoFile(`crates/${crate}/Cargo.toml`);
    assert.match(manifest, /^publish = false$/m, `${crate} is an internal boundary, not a published artifact`);
    assert.doesNotMatch(
      manifest,
      /^\s*tauri[\w-]*\s*=/m,
      `${crate} must never depend on tauri — that dependency is what the whole extraction exists to avoid`
    );
  }
});

test("the daemon crate depends on the engine, and nothing depends on the daemon", () => {
  const server = repoFile("crates/loomux-server/Cargo.toml");
  assert.match(
    server,
    /^loomux-engine = \{ path = "\.\.\/loomux-engine"/m,
    "loomux-server hosts the engine — the dependency IS the crate's reason to exist, and it is what pins the arrow's direction"
  );

  // The daemon is a leaf. If the desktop app ever grew a dependency on it, the
  // Windows binary would start linking a Linux-target daemon's dependency tree
  // — including, eventually, its WebSocket stack — straight through the
  // getrandom audit boundary CLAUDE.md constraint 2 draws around that binary.
  for (const crate of ["src-tauri", "crates/loomux-engine"]) {
    assert.doesNotMatch(
      repoFile(`${crate}/Cargo.toml`),
      /^\s*loomux-server\s*=/m,
      `${crate} must not depend on loomux-server: the daemon is a leaf, and its dependency tree is deliberately not audited for the shipped Windows binary`
    );
  }
});

test("every build-output path agrees on the workspace-root target/", () => {
  const gitignore = repoFile(".gitignore");
  assert.match(gitignore, /^target\/$/m, ".gitignore must ignore the workspace-root target/");
  assert.doesNotMatch(gitignore, /^src-tauri\/target\/$/m, "the src-tauri/target/ ignore is stale — cargo writes to the workspace root now");

  // Scoped to the assignment lines themselves: both files legitimately mention
  // src-tauri in prose nearby, and a whole-file substring check would pass or
  // fail on comment text rather than on the paths that are actually consumed.
  //
  // The executable's basename comes from the manifest, not from a literal here.
  // This test is about the DIRECTORY (workspace-root `target/`, not
  // `src-tauri/target/`), and hardcoding the name would make the next rename
  // redden it for a reason it does not police — `test/bundleidentity.test.ts`
  // owns the name at these same two sites.
  const exe = escapeRe(`${appPackageName()}.exe`);
  const ci = repoFile(".github/workflows/ci.yml");
  const ciExe = ci.match(/^\s*LOOMUX_E2E_EXE:.*$/m);
  assert.ok(ciExe, "ci.yml's e2e job must set LOOMUX_E2E_EXE");
  assert.match(
    ciExe[0],
    new RegExp(`workspace\\s*}}\\\\target\\\\debug\\\\${exe}$`),
    `LOOMUX_E2E_EXE must point at the workspace-root target/: ${ciExe[0].trim()}`
  );

  const fixtures = repoFile("e2e/fixtures.ts");
  const defaultExe = fixtures.match(/^const DEFAULT_EXE = .*$/m);
  assert.ok(defaultExe, "e2e/fixtures.ts must keep a DEFAULT_EXE fallback");
  assert.match(
    defaultExe[0],
    new RegExp(`"\\.\\./target/debug/${exe}"`),
    `DEFAULT_EXE must resolve to the workspace-root target/ — it is what runs when LOOMUX_E2E_EXE is unset: ${defaultExe[0]}`
  );

  // vite.config.ts is the fourth consumer of the workspace-root target/, and the one that
  // broke: the dev-server watch-ignore must exclude it, or a fresh `tauri dev` races cargo
  // writing a build script and dies with EBUSY (#989). The stale `**/src-tauri/**`-only
  // ignore did not match the root target/ — exactly the silent drift this test exists to catch.
  // Strip line comments before matching, so a commented-out config can't satisfy the
  // pin — the same discipline the LOOMUX_E2E_EXE/DEFAULT_EXE assertions above already use
  // by scoping to the live assignment line rather than the whole file (#989 review).
  const vite = repoFile("vite.config.ts").replace(/^[ \t]*\/\/.*$/gm, "");
  const viteIgnored = vite.match(/ignored:\s*\[([\s\S]*?)\]/);
  assert.ok(viteIgnored, "vite.config.ts must set server.watch.ignored");
  assert.match(
    viteIgnored[1],
    /["'`]\*\*\/target\/\*\*["'`]/,
    `vite.config.ts's server.watch.ignored must exclude the workspace-root target/ (#989): ${viteIgnored[1].trim()}`
  );
});

test("CI compiles the whole workspace and caches the right directory", () => {
  const ci = repoFile(".github/workflows/ci.yml");
  const release = repoFile(".github/workflows/release.yml");

  // Without --workspace, cargo builds only the package in the invocation
  // directory: crates/loomux-engine would never be compiled by CI at all, and
  // a broken engine manifest would merge green.
  assert.doesNotMatch(
    ci,
    /run: cargo (check|test) --locked(?! --workspace)/,
    "every CI cargo invocation must pass --workspace, or the engine crate is never built"
  );
  assert.match(ci, /run: cargo check --locked --workspace/, "CI must check the whole workspace");
  assert.match(ci, /run: cargo test --locked --workspace/, "CI must test the whole workspace");

  for (const [name, text] of [["ci.yml", ci], ["release.yml", release]] as const) {
    assert.doesNotMatch(
      text,
      /^\s*workspaces: src-tauri\s*$/m,
      `${name}: rust-cache must not point at src-tauri — Cargo.lock and target/ are at the repo root now, so it would cache nothing, silently`
    );
    assert.match(text, /^\s*workspaces: \.\s*$/m, `${name}: rust-cache must point at the workspace root`);
  }
});

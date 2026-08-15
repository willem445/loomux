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

test("the workspace root manifest declares both members and resolver 2", () => {
  assert.ok(repoHas("Cargo.toml"), "the Cargo workspace root manifest must exist at the repo root");
  const root = repoFile("Cargo.toml");

  assert.match(root, /^\[workspace\]$/m, "the root manifest must be a workspace manifest");
  // Explicit, because a VIRTUAL manifest does not inherit the resolver implied
  // by a member's edition — cargo warns and silently falls back to resolver 1,
  // which unifies features across normal and build dependencies differently.
  // That is the ground the getrandom audit in src-tauri/Cargo.toml stands on.
  assert.match(root, /^resolver = "2"$/m, "the workspace must state resolver 2 explicitly");

  for (const member of ["src-tauri", "crates/loomux-engine"]) {
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

test("the lockfile's loomux entry is not shadowed by its loomux-engine sibling", () => {
  const lock = repoFile("Cargo.lock");
  const appVersion = JSON.parse(repoFile("package.json")).version;

  assert.equal(
    lockVersionOf(lock, "loomux"),
    appVersion,
    "the `loomux` entry in the root Cargo.lock must carry the release version — this is the field scripts/check-versions.js reads"
  );
  // `loomux` is a strict prefix of `loomux-engine`, so any prefix/substring
  // match in the version checker would read the wrong entry. Pinning that the
  // two versions genuinely differ keeps that hazard testable rather than
  // theoretical.
  assert.equal(
    lockVersionOf(lock, "loomux-engine"),
    "0.0.0",
    "loomux-engine is publish=false and deliberately pinned at 0.0.0, outside the release version set"
  );
  assert.notEqual(
    lockVersionOf(lock, "loomux-engine"),
    appVersion,
    "the two lock entries must stay distinguishable, so a loose name match in check-versions.js fails loudly instead of silently agreeing"
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

test("the engine crate is publish=false and declares no tauri dependency", () => {
  const manifest = repoFile("crates/loomux-engine/Cargo.toml");
  assert.match(manifest, /^publish = false$/m, "loomux-engine is an internal boundary, not a published artifact");
  // The manifest-level half of the rule. Plan-463 slice A4 adds the CI step
  // that denies tauri from the crate's resolved `cargo tree`; this catches the
  // straightforward way it would get there in the meantime, and costs nothing.
  assert.doesNotMatch(
    manifest,
    /^\s*tauri[\w-]*\s*=/m,
    "loomux-engine must never depend on tauri — a core that needs webkit2gtk to build defeats the entire extraction (src-tauri depends on the engine; the arrow never points back)"
  );
});

test("every build-output path agrees on the workspace-root target/", () => {
  const gitignore = repoFile(".gitignore");
  assert.match(gitignore, /^target\/$/m, ".gitignore must ignore the workspace-root target/");
  assert.doesNotMatch(gitignore, /^src-tauri\/target\/$/m, "the src-tauri/target/ ignore is stale — cargo writes to the workspace root now");

  // Scoped to the assignment lines themselves: both files legitimately mention
  // src-tauri in prose nearby, and a whole-file substring check would pass or
  // fail on comment text rather than on the paths that are actually consumed.
  const ci = repoFile(".github/workflows/ci.yml");
  const ciExe = ci.match(/^\s*LOOMUX_E2E_EXE:.*$/m);
  assert.ok(ciExe, "ci.yml's e2e job must set LOOMUX_E2E_EXE");
  assert.match(
    ciExe[0],
    /workspace\s*}}\\target\\debug\\loomux\.exe$/,
    `LOOMUX_E2E_EXE must point at the workspace-root target/: ${ciExe[0].trim()}`
  );

  const fixtures = repoFile("e2e/fixtures.ts");
  const defaultExe = fixtures.match(/^const DEFAULT_EXE = .*$/m);
  assert.ok(defaultExe, "e2e/fixtures.ts must keep a DEFAULT_EXE fallback");
  assert.match(
    defaultExe[0],
    /"\.\.\/target\/debug\/loomux\.exe"/,
    `DEFAULT_EXE must resolve to the workspace-root target/ — it is what runs when LOOMUX_E2E_EXE is unset: ${defaultExe[0]}`
  );

  // vite.config.ts is the fourth consumer of the workspace-root target/, and the one that
  // broke: the dev-server watch-ignore must exclude it, or a fresh `tauri dev` races cargo
  // writing a build script and dies with EBUSY (#989). The stale `**/src-tauri/**`-only
  // ignore did not match the root target/ — exactly the silent drift this test exists to catch.
  const vite = repoFile("vite.config.ts");
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

// One repo slug, everywhere (#1153 phase 5).
//
// The GitHub repo rename is a human step this branch cannot perform — #1153
// calls it "a human-only action" and never names the target slug — so the ~73
// places that hardcode `willem445/loomux` have to move in the human's change,
// not in ours. The review that produced this test found the failure mode: a PR
// body called those edits "cosmetic", which is true of most of them and false
// of the two that matter.
//
// It is false in two different ways, and neither announces itself:
//
//   - **GitHub Pages does not redirect.** GitHub's own rename docs: "All
//     existing information, with the exception of project site URLs, is
//     automatically redirected to the new name." So every
//     `willem445.github.io/loomux/...` link and `docs/_config.yml`'s `baseurl`
//     404 the instant the repo is renamed. Nothing errors at rename time; the
//     published docs site just breaks.
//   - **npm checks `repository.url` for an exact match.** From npm's
//     trusted-publishing troubleshooting: "To publish from GitHub, your
//     package's `repository.url` field in `package.json` must exactly match
//     your GitHub repository." A redirect satisfies a browser and not this, so
//     a lagging manifest fails the first OIDC release with the rename long
//     since forgotten. `npm trust` reads the same field when `--repository` is
//     omitted, so it also mis-points the binding.
//
// Both are invisible at the moment the mistake is made and expensive later,
// which is exactly what a guard is for. The rule is deliberately stronger than
// "the two that matter must move": ONE slug, everywhere, so nobody has to
// re-derive which class a given site is in. `npm/package.json`'s
// `repository.url` is the source of truth because it is the field npm itself
// validates against.
//
// See doc/design/rebrand-external.md, "The human runbook".
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync, readdirSync, statSync } from "node:fs";
import { dirname, join, relative } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..");

// Directories that are not ours, or are build output.
const SKIP_DIRS = new Set([
  ".git",
  ".scratch",
  "node_modules",
  "target",
  "dist",
  "test-results",
  "playwright-report",
]);

// Lockfiles carry dependency URLs by the thousand and none of them is ours;
// binary-ish files are not text to scan.
const SKIP_FILES = new Set(["package-lock.json", "Cargo.lock"]);
const TEXT = /\.(md|json|ya?ml|ts|js|cjs|mjs|rs|toml|sh|ps1|html|css)$/;

/**
 * Slugs that are deliberately NOT this repo, each with the reason it is here.
 * Default-deny: anything not listed is a finding. A row whose slug no longer
 * appears anywhere is asserted stale below, so this list cannot quietly rot
 * into a blanket exemption.
 */
const ALLOW: Array<[slug: string, why: string]> = [];

function walk(dir: string, out: string[] = []): string[] {
  for (const entry of readdirSync(dir)) {
    if (SKIP_DIRS.has(entry)) continue;
    const p = join(dir, entry);
    if (statSync(p).isDirectory()) walk(p, out);
    else if (TEXT.test(entry) && !SKIP_FILES.has(entry)) out.push(p);
  }
  return out;
}

/** `git+https://github.com/willem445/loomux.git` -> `willem445` / `loomux`. */
function sourceOfTruth(): { owner: string; slug: string } {
  const pkg = JSON.parse(readFileSync(join(ROOT, "npm/package.json"), "utf8"));
  const url: string = pkg.repository?.url ?? "";
  const m = /github\.com\/([A-Za-z0-9._-]+)\/([A-Za-z0-9._-]+?)(?:\.git)?$/.exec(url);
  assert.ok(
    m,
    `npm/package.json's repository.url must name a GitHub repo — npm matches it exactly ` +
      `when publishing from Actions, and falls back to it when binding a trusted ` +
      `publisher. Got: ${JSON.stringify(url)}`
  );
  return { owner: m![1], slug: m![2] };
}

test("every hardcoded repo slug agrees with npm/package.json's repository.url", () => {
  const { owner, slug } = sourceOfTruth();

  // Three shapes, tracked separately so a pattern that stops matching anything
  // is caught as a vacuous scan rather than passing as "no offenders".
  const patterns: Array<[kind: string, re: RegExp]> = [
    ["github.com link", new RegExp(`github\\.com/${owner}/([A-Za-z0-9._-]+?)(?=[.\\s/)"'\`,;]|\\.git\\b|$)`, "g")],
    ["Pages link", new RegExp(`${owner}\\.github\\.io/([A-Za-z0-9._-]+)`, "g")],
    ["Jekyll baseurl", /^\s*baseurl:\s*\/([A-Za-z0-9._-]+)/gm],
  ];

  const seen = new Map<string, number>(); // kind -> count
  const offenders: string[] = [];
  const allowedHit = new Set<string>();

  for (const file of walk(ROOT)) {
    const rel = relative(ROOT, file).replace(/\\/g, "/");
    const src = readFileSync(file, "utf8");
    const lines = src.split(/\r?\n/);
    for (const [kind, re] of patterns) {
      // `baseurl` is a Jekyll config key; only _config.yml declares one.
      if (kind === "Jekyll baseurl" && !rel.endsWith("_config.yml")) continue;
      lines.forEach((line, i) => {
        for (const m of line.matchAll(new RegExp(re.source, re.flags.replace("m", "")))) {
          const found = m[1].replace(/\.git$/, "");
          seen.set(kind, (seen.get(kind) ?? 0) + 1);
          const allow = ALLOW.find(([s]) => s === found);
          if (allow) {
            allowedHit.add(found);
            continue;
          }
          if (found !== slug) {
            offenders.push(`${rel}:${i + 1}: ${kind} names "${found}", not "${slug}" — ${line.trim()}`);
          }
        }
      });
    }
  }

  // Positive controls. An absence-only assertion passes just as well when the
  // scan never ran, and two of these three patterns are the ones that matter
  // most — so each must have a live witness or this test is decoration.
  for (const [kind] of patterns) {
    assert.ok(
      (seen.get(kind) ?? 0) > 0,
      `the "${kind}" pattern matched nothing at all — the scan is vacuous on the shape ` +
        `it exists to police, so a mismatch there would pass silently`
    );
  }

  // A stale allowlist row is a claim nobody re-checked.
  for (const [s, why] of ALLOW) {
    assert.ok(
      allowedHit.has(s),
      `the allowlist exempts "${s}" (${why}) but nothing in the tree names it any more — ` +
        `drop the row rather than leaving an unexamined exemption behind`
    );
  }

  assert.deepEqual(
    offenders,
    [],
    `the repo slug must be spelled the same everywhere. GitHub redirects most of these, ` +
      `but NOT project-site (Pages) URLs, and npm requires repository.url to match exactly ` +
      `— so a partial rename breaks the docs site and the first OIDC publish, silently and ` +
      `much later. Move them together, or add an argued row to ALLOW.\n` +
      offenders.join("\n")
  );
});

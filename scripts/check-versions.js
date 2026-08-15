#!/usr/bin/env node
// Mechanical backstop for release bumps (#274): asserts every file that
// carries the loomux version string agrees. v0.8.0 shipped with a stale
// 0.7.1 package-lock.json (#224) because that step was checklist-only —
// this makes drift a CI failure instead of relying on discipline.
//
// Dependency-free by design: node ships on every CI runner and parses JSON
// natively, so Cargo.toml/Cargo.lock (not JSON) get a light line scan
// instead of pulling in a TOML parser.
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const root = path.resolve(__dirname, '..');

function readJson(relPath) {
  return JSON.parse(fs.readFileSync(path.join(root, relPath), 'utf8'));
}

function readText(relPath) {
  return fs.readFileSync(path.join(root, relPath), 'utf8');
}

// Finds `version = "X.Y.Z"` on the first line at column 0 after a
// `[package]` header — good enough for the one manifest this is ever pointed
// at (src-tauri/Cargo.toml), and avoids matching a dependency's
// `version = "..."` field.
//
// The repo became a Cargo workspace in #888 slice A1, so there are now other
// package manifests in the tree. Only src-tauri's carries the loomux release
// version: every `crates/loomux-*` member is `publish = false` and pinned at
// 0.0.0 on purpose (see each Cargo.toml), so none of them is in this check.
// Written as the rule rather than as a list of the members that exist today —
// there were two at slice A1 and three from C1a, and a count here is one more
// thing a new member has to remember to update.
function cargoTomlVersion(relPath) {
  const text = readText(relPath);
  const lines = text.split(/\r?\n/);
  let inPackage = false;
  for (const line of lines) {
    if (/^\[.*\]\s*$/.test(line)) {
      inPackage = line.trim() === '[package]';
      continue;
    }
    if (inPackage) {
      const m = line.match(/^version\s*=\s*"([^"]+)"/);
      if (m) return m[1];
    }
  }
  throw new Error(`could not find [package] version in ${relPath}`);
}

// Finds the `version = "X.Y.Z"` line inside the `name = "loomux"` package
// entry in Cargo.lock.
//
// The exact-equality test (not `startsWith`/`includes`) is load-bearing since
// the workspace conversion: `loomux` is a strict PREFIX of every sibling entry
// in the same file (`loomux-engine`, `loomux-server`, and whatever joins next),
// and a looser match would read ONE OF THEIR versions — a permanent 0.0.0 — and
// report a mismatch on every correctly-bumped release. test/workspacelayout.test.ts
// re-implements this rule and asserts it over each sibling, so the hazard grows
// with the workspace instead of with this comment.
function cargoLockVersion(relPath) {
  const text = readText(relPath);
  const lines = text.split(/\r?\n/);
  for (let i = 0; i < lines.length; i++) {
    if (lines[i].trim() === 'name = "loomux"') {
      const m = lines[i + 1] && lines[i + 1].match(/^version\s*=\s*"([^"]+)"/);
      if (m) return m[1];
    }
  }
  throw new Error(`could not find the loomux package entry in ${relPath}`);
}

function collectVersions() {
  const pkgLock = readJson('package-lock.json');
  return [
    { file: 'package.json', field: 'version', version: readJson('package.json').version },
    { file: 'package-lock.json', field: 'version', version: pkgLock.version },
    {
      file: 'package-lock.json',
      field: 'packages[""].version',
      version: pkgLock.packages && pkgLock.packages[''] && pkgLock.packages[''].version,
    },
    { file: 'npm/package.json', field: 'version', version: readJson('npm/package.json').version },
    {
      file: 'src-tauri/tauri.conf.json',
      field: 'version',
      version: readJson('src-tauri/tauri.conf.json').version,
    },
    {
      file: 'src-tauri/Cargo.toml',
      field: '[package] version',
      version: cargoTomlVersion('src-tauri/Cargo.toml'),
    },
    {
      // Workspace root since #888 slice A1 — cargo keeps ONE lockfile per
      // workspace, and it lives next to the root manifest. A stale
      // src-tauri/Cargo.lock left behind would be a file cargo never updates
      // and this check would happily read; test/workspacelayout.test.ts
      // asserts it is gone.
      file: 'Cargo.lock',
      field: 'loomux package version',
      version: cargoLockVersion('Cargo.lock'),
    },
  ];
}

function main() {
  const entries = collectVersions();
  const versions = new Set(entries.map((e) => e.version));

  const report = entries.map((e) => `  ${e.version ?? '<missing>'}  ${e.file} (${e.field})`).join('\n');

  if (versions.size > 1) {
    console.error('Version mismatch across release files:\n' + report);
    console.error(
      '\nAll seven version fields across six files (see .claude/skills/release/SKILL.md) must agree. ' +
        'Bump every file together, then re-run `npm run check:versions`.',
    );
    process.exitCode = 1;
    return;
  }

  console.log(`Version sources agree at ${entries[0].version}:\n${report}`);
}

main();

#!/usr/bin/env node
'use strict';
// code-metrics — a REPORT on the shape of this repo's own code (#2138, slices A+B
// of the #2128 investigation).
//
// WHAT THIS ANSWERS. Per language: how long functions are, how deeply they nest,
// how many arguments they take, which TypeScript modules form import cycles, which
// exports nobody imports, and how much of each root is comment or blank. Per pull
// request: the same numbers at the merge-base and at the head, so a reviewer reads
// a DELTA instead of a level.
//
// REPORT-ONLY, BY DESIGN. Nothing here fails. No threshold is enforced, no exit
// code carries a metric, and every parse error degrades to a missing row. The
// distributions did not exist when this was written (#2128 part 8) — the gates that
// will use them (#2128 slice C) are a separate decision, taken once these numbers
// have been watched for a while. `doc/design/code-metrics.md` is the spec.
//
// WHY A SCRIPT AND NOT PRODUCT CODE. CLAUDE.md constraint 8: orrerix is a generic
// agentic-dev tool, and "how big are the functions in THIS repo" is feedback about
// developing orrerix, not a product capability. Everything lives in repo config —
// `scripts/`, `test/`, `.github/`, `doc/design/`.
//
// NO NEW DEPENDENCY. `typescript` is already a devDependency and its compiler API
// gives the whole TS side; the Rust side is a parser for `cargo clippy
// --message-format=json`, which ships with the toolchain. Dependency-free CJS (the
// root package.json is `"type": "module"`, so a `.js` file here would be ESM and
// `require` a ReferenceError — #1181).
//
// SUBCOMMANDS
//   clippy   read `cargo clippy --message-format=json` on stdin, write a compact
//            per-platform JSON. Streaming: the raw form is hundreds of MB once the
//            thresholds are set to 1, and none of it is worth keeping.
//   report   walk the tree, merge any number of clippy files, write
//            `code-metrics.json` and a markdown summary.
//   delta    compare two `code-metrics.json` files and write the PR comment body.

const fs = require('node:fs');
const path = require('node:path');
const readline = require('node:readline');

const SCHEMA_VERSION = 1;

// The marker the sticky PR comment is found by. Part of the persisted contract:
// changing it orphans every comment already posted.
const COMMENT_MARKER = '<!-- code-metrics -->';

// ---------------------------------------------------------------------------
// Distributions
//
// Nearest-rank percentile over the sorted values (the same definition #2128 part 1
// used by hand, so the two are comparable): the p-th percentile is the value at
// rank ceil(p/100 * n), 1-indexed. No interpolation — every reported number is a
// value some real function actually has, which is what lets "the worst N functions"
// and "p95" be answered off one list.
// ---------------------------------------------------------------------------

function percentile(sortedAsc, p) {
  if (sortedAsc.length === 0) return null;
  const rank = Math.ceil((p / 100) * sortedAsc.length);
  return sortedAsc[Math.min(sortedAsc.length - 1, Math.max(0, rank - 1))];
}

function distribution(values) {
  const s = values
    .filter((v) => typeof v === 'number' && Number.isFinite(v))
    .sort((a, b) => a - b);
  return {
    n: s.length,
    p50: percentile(s, 50),
    p90: percentile(s, 90),
    p95: percentile(s, 95),
    max: s.length ? s[s.length - 1] : null,
  };
}

// ---------------------------------------------------------------------------
// Line counting
//
// A comment line is one whose first non-blank characters are `//` — the same
// definition #2128 part 1 measured with grep, so these numbers continue that
// baseline. Rust `///` doc lines are therefore comments, and a trailing `// why` on
// a code line is not. Block comments (`/* */`) are not tracked: counting them
// properly needs a lexer, and doing it here would make the number incomparable with
// the baseline it continues.
// ---------------------------------------------------------------------------

function countLines(text) {
  const lines = text.split('\n');
  // A trailing newline yields one empty final element that is not a line.
  if (lines.length && lines[lines.length - 1] === '') lines.pop();
  let comment = 0;
  let blank = 0;
  for (const raw of lines) {
    const t = raw.trim();
    if (t === '') blank += 1;
    else if (t.startsWith('//')) comment += 1;
  }
  return { lines: lines.length, comment, blank, code: lines.length - comment - blank };
}

function walk(dir, exts, out) {
  let entries;
  try {
    entries = fs.readdirSync(dir, { withFileTypes: true });
  } catch {
    return out;
  }
  for (const e of entries.sort((a, b) => (a.name < b.name ? -1 : 1))) {
    const full = path.join(dir, e.name);
    if (e.isDirectory()) {
      if (e.name === 'node_modules' || e.name === 'target' || e.name === '.git') continue;
      walk(full, exts, out);
    } else if (exts.some((x) => e.name.endsWith(x))) {
      out.push(full);
    }
  }
  return out;
}

function posix(p) {
  return String(p).split(path.sep).join('/').replace(/\\/g, '/');
}

// ---------------------------------------------------------------------------
// TypeScript: functions, nesting, the import graph, dead exports
//
// `ts.createSourceFile` per file, never a Program: nothing here needs a type
// checker, and building one over 150+ files costs seconds this job should not
// spend. The consequence is stated where it bites — dead exports are decided from
// import SPECIFIERS, so a dynamic `import()` or a consumer outside the scanned root
// can make a live export look dead. That is why the row is report-only.
// ---------------------------------------------------------------------------

function loadTypeScript() {
  try {
    return require('typescript');
  } catch {
    return null;
  }
}

function isFunctionLike(ts, n) {
  return (
    ts.isFunctionDeclaration(n) ||
    ts.isFunctionExpression(n) ||
    ts.isArrowFunction(n) ||
    ts.isMethodDeclaration(n) ||
    ts.isConstructorDeclaration(n) ||
    ts.isGetAccessorDeclaration(n) ||
    ts.isSetAccessorDeclaration(n)
  );
}

function functionName(ts, node) {
  if (node.name && ts.isIdentifier(node.name)) return node.name.text;
  if (node.name && ts.isStringLiteral(node.name)) return node.name.text;
  if (ts.isConstructorDeclaration(node)) {
    const cls = node.parent;
    const owner = cls && cls.name && cls.name.text ? cls.name.text : '<class>';
    return owner + '.constructor';
  }
  const p = node.parent;
  if (p && ts.isVariableDeclaration(p) && p.name && ts.isIdentifier(p.name)) return p.name.text;
  if (p && ts.isPropertyAssignment(p) && p.name && ts.isIdentifier(p.name)) return p.name.text;
  if (p && ts.isPropertyDeclaration(p) && p.name && ts.isIdentifier(p.name)) return p.name.text;
  if (p && ts.isExportAssignment(p)) return 'default';
  return '<anonymous>';
}

// Nesting depth INSIDE a function body, relative to the body: an `if` at the top of
// the body is depth 1. Only constructs that branch count — a bare block, an object
// literal or a class body do not, because they do not make the code harder to
// follow the way a branch does. A nested function starts its own scale; its own row
// carries its own depth.
function nestingDepth(ts, fnNode) {
  const body = fnNode.body;
  if (!body) return 0;
  let max = 0;
  const nests = (n) =>
    ts.isIfStatement(n) ||
    ts.isForStatement(n) ||
    ts.isForInStatement(n) ||
    ts.isForOfStatement(n) ||
    ts.isWhileStatement(n) ||
    ts.isDoStatement(n) ||
    ts.isSwitchStatement(n) ||
    ts.isTryStatement(n) ||
    ts.isCatchClause(n) ||
    ts.isConditionalExpression(n);
  const visit = (n, depth) => {
    if (isFunctionLike(ts, n)) return;
    const d = nests(n) ? depth + 1 : depth;
    if (d > max) max = d;
    ts.forEachChild(n, (c) => visit(c, d));
  };
  ts.forEachChild(body, (c) => visit(c, 0));
  return max;
}

// Resolve a relative import specifier to a repo-relative path. `src/` imports carry
// an explicit `.ts` extension (tsconfig's `allowImportingTsExtensions`), but the
// extensionless and `/index` forms resolve too, so an edge is not silently dropped
// if that convention ever changes.
function resolveSpecifier(fromFile, spec, known) {
  if (!spec.startsWith('.')) return null;
  const base = path.posix.join(path.posix.dirname(posix(fromFile)), spec);
  const candidates = [base, base + '.ts', base + '.tsx', base + '/index.ts'];
  for (const c of candidates) {
    const norm = path.posix.normalize(c);
    if (known.has(norm)) return norm;
  }
  return null;
}

// Directories scanned for CONSUMERS only. The import graph, the cycle census and
// every per-function number are about `src/` alone, but "nobody imports this
// export" is a claim about the whole repo: `test/` and `e2e/` import `src/` freely,
// so a dead-export list that cannot see them reports live code as dead. These
// contribute consumption edges and nothing else.
const CONSUMER_DIRS = ['test', 'e2e'];

function analyzeTypeScript(rootDir, repoRoot, consumerDirs) {
  const ts = loadTypeScript();
  const rootRel = posix(path.relative(repoRoot, rootDir));
  if (!ts) {
    return {
      root: rootRel,
      available: false,
      files: [],
      functions: [],
      edges: [],
      cycles: [],
      deadExports: [],
      percentiles: {},
    };
  }

  const absFiles = walk(rootDir, ['.ts', '.tsx'], []).filter((f) => !f.endsWith('.d.ts'));
  const rel = absFiles.map((f) => posix(path.relative(repoRoot, f)));
  const known = new Set(rel);

  const files = [];
  const functions = [];
  const edges = [];
  // target file -> Set of names some other file imports from it. `*` means a
  // namespace import, a bare side-effect import or an `export *`, any of which
  // makes every export of that file live.
  const consumed = new Map();
  const exportsByFile = new Map();

  for (let i = 0; i < absFiles.length; i += 1) {
    const relPath = rel[i];
    let text;
    try {
      text = fs.readFileSync(absFiles[i], 'utf8');
    } catch {
      continue;
    }
    const counts = countLines(text);
    const sf = ts.createSourceFile(relPath, text, ts.ScriptTarget.ES2022, true, ts.ScriptKind.TS);
    const lineOf = (pos) => sf.getLineAndCharacterOfPosition(pos).line + 1;

    const exported = [];
    const addExport = (name, line) => {
      if (name) exported.push({ name, line });
    };
    const noteConsumed = (target, names) => {
      if (!consumed.has(target)) consumed.set(target, new Set());
      const set = consumed.get(target);
      for (const n of names) set.add(n);
    };

    const visit = (node) => {
      if (isFunctionLike(ts, node) && node.body) {
        const startLine = lineOf(node.getStart(sf));
        const endLine = lineOf(node.end);
        functions.push({
          file: relPath,
          name: functionName(ts, node),
          line: startLine,
          endLine,
          lines: endLine - startLine + 1,
          depth: nestingDepth(ts, node),
          args: node.parameters ? node.parameters.length : 0,
        });
      }

      if (
        ts.isImportDeclaration(node) &&
        node.moduleSpecifier &&
        ts.isStringLiteral(node.moduleSpecifier)
      ) {
        const target = resolveSpecifier(relPath, node.moduleSpecifier.text, known);
        if (target && target !== relPath) {
          edges.push([relPath, target]);
          const c = node.importClause;
          if (!c) noteConsumed(target, ['*']);
          else {
            const names = [];
            if (c.name) names.push('default');
            if (c.namedBindings) {
              if (ts.isNamespaceImport(c.namedBindings)) names.push('*');
              else for (const el of c.namedBindings.elements) names.push((el.propertyName || el.name).text);
            }
            noteConsumed(target, names);
          }
        }
      }

      if (ts.isExportDeclaration(node)) {
        if (node.moduleSpecifier && ts.isStringLiteral(node.moduleSpecifier)) {
          const target = resolveSpecifier(relPath, node.moduleSpecifier.text, known);
          if (target && target !== relPath) {
            edges.push([relPath, target]);
            if (!node.exportClause || ts.isNamespaceExport(node.exportClause)) noteConsumed(target, ['*']);
            else noteConsumed(target, node.exportClause.elements.map((el) => (el.propertyName || el.name).text));
          }
        } else if (node.exportClause && ts.isNamedExports(node.exportClause)) {
          for (const el of node.exportClause.elements) addExport(el.name.text, lineOf(el.getStart(sf)));
        }
      }

      const mods = ts.canHaveModifiers && ts.canHaveModifiers(node) ? ts.getModifiers(node) : node.modifiers;
      if (mods && mods.some((m) => m.kind === ts.SyntaxKind.ExportKeyword)) {
        const line = lineOf(node.getStart(sf));
        if (ts.isVariableStatement(node)) {
          for (const d of node.declarationList.declarations) {
            if (ts.isIdentifier(d.name)) addExport(d.name.text, lineOf(d.getStart(sf)));
          }
        } else if (node.name && ts.isIdentifier(node.name)) {
          addExport(node.name.text, line);
        } else if (mods.some((m) => m.kind === ts.SyntaxKind.DefaultKeyword)) {
          addExport('default', line);
        }
      }

      ts.forEachChild(node, visit);
    };
    ts.forEachChild(sf, visit);

    exportsByFile.set(relPath, exported);
    files.push(Object.assign({ file: relPath }, counts, { exports: exported.length }));
  }

  // Second pass: consumers outside the analyzed root. Their imports feed `consumed`
  // (so an export used only by a test is not "dead") and nothing else — no edge, no
  // function row, no cycle membership.
  for (const dir of consumerDirs === undefined ? CONSUMER_DIRS : consumerDirs) {
    const absDir = path.join(repoRoot, dir);
    for (const abs of walk(absDir, ['.ts', '.tsx'], [])) {
      const relPath = posix(path.relative(repoRoot, abs));
      if (known.has(relPath)) continue;
      let text;
      try {
        text = fs.readFileSync(abs, 'utf8');
      } catch {
        continue;
      }
      const sf = ts.createSourceFile(relPath, text, ts.ScriptTarget.ES2022, true, ts.ScriptKind.TS);
      const note = (target, names) => {
        if (!consumed.has(target)) consumed.set(target, new Set());
        const set = consumed.get(target);
        for (const n of names) set.add(n);
      };
      const visit = (node) => {
        const spec =
          (ts.isImportDeclaration(node) || ts.isExportDeclaration(node)) &&
          node.moduleSpecifier &&
          ts.isStringLiteral(node.moduleSpecifier)
            ? node.moduleSpecifier.text
            : null;
        if (spec) {
          const target = resolveSpecifier(relPath, spec, known);
          if (target) {
            if (ts.isImportDeclaration(node)) {
              const c = node.importClause;
              if (!c) note(target, ['*']);
              else {
                const names = [];
                if (c.name) names.push('default');
                if (c.namedBindings) {
                  if (ts.isNamespaceImport(c.namedBindings)) names.push('*');
                  else for (const el of c.namedBindings.elements) names.push((el.propertyName || el.name).text);
                }
                note(target, names);
              }
            } else if (!node.exportClause || ts.isNamespaceExport(node.exportClause)) note(target, ['*']);
            else note(target, node.exportClause.elements.map((el) => (el.propertyName || el.name).text));
          }
        }
        ts.forEachChild(node, visit);
      };
      ts.forEachChild(sf, visit);
    }
  }

  const deadExports = [];
  for (const [file, exported] of exportsByFile) {
    const set = consumed.get(file) || new Set();
    if (set.has('*')) continue;
    for (const e of exported) {
      if (!set.has(e.name)) deadExports.push({ file, name: e.name, line: e.line });
    }
  }

  const fanOut = new Map();
  const fanIn = new Map();
  for (const [from, to] of edges) {
    fanOut.set(from, (fanOut.get(from) || 0) + 1);
    fanIn.set(to, (fanIn.get(to) || 0) + 1);
  }
  const deadByFile = new Map();
  for (const d of deadExports) deadByFile.set(d.file, (deadByFile.get(d.file) || 0) + 1);
  for (const f of files) {
    f.fanOut = fanOut.get(f.file) || 0;
    f.fanIn = fanIn.get(f.file) || 0;
    f.deadExports = deadByFile.get(f.file) || 0;
  }

  return {
    root: rootRel,
    available: true,
    files,
    functions,
    edges: edges.map((e) => e[0] + ' -> ' + e[1]),
    cycles: stronglyConnected(rel, edges),
    deadExports,
    percentiles: {
      fnLines: distribution(functions.map((f) => f.lines)),
      depth: distribution(functions.map((f) => f.depth)),
      args: distribution(functions.map((f) => f.args)),
      fileLines: distribution(files.map((f) => f.lines)),
    },
  };
}

// ---------------------------------------------------------------------------
// Tarjan's strongly-connected components — the cycle census.
//
// Iterative, not recursive: 150-odd nodes is small, but the same code runs over
// `test/` too and a stack overflow would be a failure mode in a job whose whole
// promise is that it never fails. A component is reported when it has more than one
// member, or when a node imports itself.
// ---------------------------------------------------------------------------

function stronglyConnected(nodes, edgeList) {
  const adj = new Map();
  for (const n of nodes) adj.set(n, []);
  const selfLoops = new Set();
  for (const [a, b] of edgeList) {
    if (a === b) selfLoops.add(a);
    if (!adj.has(a)) adj.set(a, []);
    adj.get(a).push(b);
  }
  const index = new Map();
  const low = new Map();
  const onStack = new Set();
  const stack = [];
  const out = [];
  let counter = 0;

  for (const root of adj.keys()) {
    if (index.has(root)) continue;
    // Each frame is [node, next-child-cursor].
    const work = [[root, 0]];
    index.set(root, counter);
    low.set(root, counter);
    counter += 1;
    stack.push(root);
    onStack.add(root);
    while (work.length) {
      const frame = work[work.length - 1];
      const v = frame[0];
      const i = frame[1];
      const kids = adj.get(v) || [];
      if (i < kids.length) {
        frame[1] = i + 1;
        const w = kids[i];
        if (!index.has(w)) {
          index.set(w, counter);
          low.set(w, counter);
          counter += 1;
          stack.push(w);
          onStack.add(w);
          work.push([w, 0]);
        } else if (onStack.has(w)) {
          low.set(v, Math.min(low.get(v), index.get(w)));
        }
        continue;
      }
      work.pop();
      if (work.length) {
        const parent = work[work.length - 1][0];
        low.set(parent, Math.min(low.get(parent), low.get(v)));
      }
      if (low.get(v) === index.get(v)) {
        const comp = [];
        for (;;) {
          const w = stack.pop();
          onStack.delete(w);
          comp.push(w);
          if (w === v) break;
        }
        if (comp.length > 1 || selfLoops.has(v)) out.push(comp.sort());
      }
    }
  }
  return out.sort((a, b) => (a.join() < b.join() ? -1 : 1));
}

// ---------------------------------------------------------------------------
// Rust, through clippy's JSON
//
// The lints that carry the numbers (`too_many_lines`, `cognitive_complexity`,
// `too_many_arguments`) are allow-by-default and threshold-gated, so CI runs them
// with `--force-warn` against `.github/clippy/clippy.toml`, whose thresholds are 1.
// Every function then emits its own value and this parser reads it out of the
// message text. `--force-warn` rather than `-W` on purpose: 20 `allow(clippy::…)`
// attributes already exist in the tree and `-W` would leave those functions
// unmeasured (#2128 part 3).
//
// The lint's own message is the only place the VALUE appears — the JSON has no
// numeric field for it — so these patterns are the contract with clippy's wording.
// A pattern that stops matching produces a missing row, never a wrong number, and
// `messagesSeen` vs `parsed` in the output says so out loud.
// ---------------------------------------------------------------------------

const CLIPPY_VALUE_PATTERNS = [
  ['lines', /^clippy::too_many_lines$/, /has too many lines \((\d+)\/\d+\)/],
  ['cognitive', /^clippy::cognitive_complexity$/, /cognitive complexity of \((\d+)\/\d+\)/],
  ['args', /^clippy::too_many_arguments$/, /has too many arguments \((\d+)\/\d+\)/],
];

const CLIPPY_COUNT_LINTS = new Map([
  ['clippy::unwrap_used', 'unwrap'],
  ['clippy::expect_used', 'expect'],
  ['clippy::panic', 'panic'],
]);

function newClippyAccumulator(platform) {
  return {
    platform: platform || 'unknown',
    // key: file:line -> row. One function emits up to three lints at the same span;
    // they are merged so a row carries every number that fired for it.
    fns: new Map(),
    perFile: new Map(),
    messagesSeen: 0,
    parsed: 0,
    unparsed: [],
  };
}

function clippyAddMessage(acc, msg) {
  if (!msg || typeof msg !== 'object') return;
  const code = msg.code && msg.code.code;
  if (!code) return;
  const spans = Array.isArray(msg.spans) ? msg.spans : [];
  const primary = spans.find((s) => s && s.is_primary) || spans[0];
  if (!primary || !primary.file_name) return;
  const file = posix(primary.file_name);
  const line = primary.line_start || 0;
  acc.messagesSeen += 1;

  const counted = CLIPPY_COUNT_LINTS.get(code);
  if (counted) {
    if (!acc.perFile.has(file)) acc.perFile.set(file, { unwrap: 0, expect: 0, panic: 0 });
    acc.perFile.get(file)[counted] += 1;
    acc.parsed += 1;
    return;
  }

  for (const [field, codeRe, valueRe] of CLIPPY_VALUE_PATTERNS) {
    if (!codeRe.test(code)) continue;
    const m = valueRe.exec(String(msg.message || ''));
    if (!m) {
      if (acc.unparsed.length < 20) acc.unparsed.push({ code, message: String(msg.message || '').slice(0, 120) });
      return;
    }
    const key = file + ':' + line;
    if (!acc.fns.has(key)) {
      acc.fns.set(key, {
        file,
        line,
        endLine: primary.line_end || line,
        name: null,
        lines: null,
        cognitive: null,
        args: null,
      });
    }
    const row = acc.fns.get(key);
    // Two build legs, or two crates in one workspace, can report the same span; the
    // larger value wins so a `cfg`-gated body is never under-reported.
    row[field] = row[field] === null ? Number(m[1]) : Math.max(row[field], Number(m[1]));
    if (primary.line_end && primary.line_end > row.endLine) row.endLine = primary.line_end;
    acc.parsed += 1;
    return;
  }
}

// Clippy's spans do not carry the function's NAME, so it is read back out of the
// source at the span's start. Falls back to `<file>:<line>` — a row with no name is
// still a row with a number.
function nameFunctionsFromSource(acc, repoRoot) {
  const cache = new Map();
  for (const row of acc.fns.values()) {
    if (row.name) continue;
    let lines = cache.get(row.file);
    if (lines === undefined) {
      try {
        lines = fs.readFileSync(path.join(repoRoot, row.file), 'utf8').split(/\r?\n/);
      } catch {
        lines = null;
      }
      cache.set(row.file, lines);
    }
    let found = null;
    if (lines) {
      for (let i = row.line - 1; i < Math.min(lines.length, row.line + 4); i += 1) {
        const m = /\bfn\s+([A-Za-z_][A-Za-z0-9_]*)/.exec(lines[i] || '');
        if (m) {
          found = m[1];
          break;
        }
      }
    }
    row.name = found || row.file + ':' + row.line;
  }
}

function clippyFinish(acc) {
  return {
    platform: acc.platform,
    messagesSeen: acc.messagesSeen,
    parsed: acc.parsed,
    unparsed: acc.unparsed,
    functions: Array.from(acc.fns.values()).sort((a, b) =>
      a.file === b.file ? a.line - b.line : a.file < b.file ? -1 : 1
    ),
    perFile: Array.from(acc.perFile.entries())
      .map((e) => Object.assign({ file: e[0] }, e[1]))
      .sort((a, b) => (a.file < b.file ? -1 : 1)),
  };
}

// Parse a whole `cargo clippy --message-format=json` text (the form the fixture
// test uses). The streaming form in `cmdClippy` shares `clippyAddMessage`.
function parseClippyText(text, platform, repoRoot) {
  const acc = newClippyAccumulator(platform);
  for (const line of text.split('\n')) {
    const t = line.trim();
    if (!t.startsWith('{')) continue;
    let obj;
    try {
      obj = JSON.parse(t);
    } catch {
      continue;
    }
    if (obj.reason !== 'compiler-message') continue;
    clippyAddMessage(acc, obj.message);
  }
  if (repoRoot) nameFunctionsFromSource(acc, repoRoot);
  else for (const row of acc.fns.values()) if (!row.name) row.name = row.file + ':' + row.line;
  return clippyFinish(acc);
}

// Merge the per-leg clippy files into one Rust view. A function present on several
// legs keeps the largest value for each metric: a `cfg(windows)` body is invisible
// on ubuntu, so the union across legs is the only complete picture (#2128 part 5).
function mergeClippy(legs) {
  const fns = new Map();
  const perFile = new Map();
  const platforms = [];
  let messagesSeen = 0;
  let parsed = 0;
  for (const leg of legs) {
    if (!leg) continue;
    platforms.push(leg.platform || 'unknown');
    messagesSeen += leg.messagesSeen || 0;
    parsed += leg.parsed || 0;
    for (const f of leg.functions || []) {
      const key = f.file + '#' + (f.name || f.line);
      const cur = fns.get(key);
      if (!cur) {
        fns.set(key, Object.assign({}, f));
        continue;
      }
      for (const k of ['lines', 'cognitive', 'args']) {
        if (f[k] !== null && f[k] !== undefined) cur[k] = cur[k] === null || cur[k] === undefined ? f[k] : Math.max(cur[k], f[k]);
      }
    }
    for (const r of leg.perFile || []) {
      const cur = perFile.get(r.file);
      if (!cur) perFile.set(r.file, { file: r.file, unwrap: r.unwrap, expect: r.expect, panic: r.panic });
      else {
        cur.unwrap = Math.max(cur.unwrap, r.unwrap);
        cur.expect = Math.max(cur.expect, r.expect);
        cur.panic = Math.max(cur.panic, r.panic);
      }
    }
  }
  const functions = Array.from(fns.values()).sort((a, b) =>
    a.file === b.file ? a.line - b.line : a.file < b.file ? -1 : 1
  );
  const files = Array.from(perFile.values()).sort((a, b) => (a.file < b.file ? -1 : 1));
  return {
    available: platforms.length > 0,
    platforms,
    messagesSeen,
    parsed,
    functions,
    perFile: files,
    totals: {
      unwrap: files.reduce((s, r) => s + r.unwrap, 0),
      expect: files.reduce((s, r) => s + r.expect, 0),
      panic: files.reduce((s, r) => s + r.panic, 0),
    },
    percentiles: {
      fnLines: distribution(functions.map((f) => f.lines).filter((v) => v !== null)),
      cognitive: distribution(functions.map((f) => f.cognitive).filter((v) => v !== null)),
      args: distribution(functions.map((f) => f.args).filter((v) => v !== null)),
    },
  };
}

// ---------------------------------------------------------------------------
// Roots: lines / comment / blank, and the file-size distribution
// ---------------------------------------------------------------------------

const DEFAULT_ROOTS = [
  { name: 'src', dir: 'src', exts: ['.ts'] },
  { name: 'test', dir: 'test', exts: ['.ts'] },
  { name: 'e2e', dir: 'e2e', exts: ['.ts'] },
  { name: 'src-tauri/src', dir: 'src-tauri/src', exts: ['.rs'] },
  { name: 'src-tauri/tests', dir: 'src-tauri/tests', exts: ['.rs'] },
  { name: 'crates/loomux-engine/src', dir: 'crates/loomux-engine/src', exts: ['.rs'] },
  { name: 'crates/loomux-server/src', dir: 'crates/loomux-server/src', exts: ['.rs'] },
];

function rootTotals(repoRoot, roots) {
  const out = {};
  for (const r of roots) {
    const abs = path.join(repoRoot, r.dir);
    const files = walk(abs, r.exts, []);
    let lines = 0;
    let comment = 0;
    let blank = 0;
    const sizes = [];
    for (const f of files) {
      let c;
      try {
        c = countLines(fs.readFileSync(f, 'utf8'));
      } catch {
        continue;
      }
      lines += c.lines;
      comment += c.comment;
      blank += c.blank;
      sizes.push(c.lines);
    }
    out[r.name] = {
      files: sizes.length,
      lines,
      comment,
      blank,
      commentShare: lines ? Number((comment / lines).toFixed(4)) : 0,
      fileLines: distribution(sizes),
    };
  }
  return out;
}

// ---------------------------------------------------------------------------
// The diff view: what the PR ADDED
//
// Fed a unified diff (`git diff <base>...<head>`), so the "new unwrap" and "comment
// share of added lines" rows are about added lines only and existing debt never
// shows up. Parsing the diff here rather than in the workflow keeps the arithmetic
// in the file the fixture test can reach.
// ---------------------------------------------------------------------------

function analyzeDiff(diffText) {
  const out = {
    files: 0,
    addedLines: 0,
    addedComment: 0,
    addedUnwrap: [],
    addedExpect: [],
    addedPanic: [],
    addedAllow: [],
  };
  out.commentShare = 0;
  if (!diffText) return out;
  let file = null;
  let line = 0;
  const isProductRust = (f) =>
    f && f.endsWith('.rs') && (f.startsWith('src-tauri/src/') || /^crates\/[^/]+\/src\//.test(f));
  for (const raw of diffText.split('\n')) {
    if (raw.startsWith('+++ ')) {
      const p = raw.slice(4).trim();
      file = p === '/dev/null' ? null : posix(p.replace(/^b\//, ''));
      if (file) out.files += 1;
      continue;
    }
    const hunk = /^@@ -\d+(?:,\d+)? \+(\d+)(?:,\d+)? @@/.exec(raw);
    if (hunk) {
      line = Number(hunk[1]);
      continue;
    }
    if (!file) continue;
    if (raw.startsWith('+') && !raw.startsWith('+++')) {
      const body = raw.slice(1);
      const t = body.trim();
      out.addedLines += 1;
      if (t.startsWith('//')) out.addedComment += 1;
      const at = { file, line, text: t.slice(0, 160) };
      if (isProductRust(file)) {
        if (/\.unwrap\(\)/.test(body)) out.addedUnwrap.push(at);
        if (/\.expect\(/.test(body)) out.addedExpect.push(at);
        if (/\bpanic!\(/.test(body)) out.addedPanic.push(at);
        if (/allow\(clippy::/.test(body)) out.addedAllow.push(at);
      }
      line += 1;
    } else if (!raw.startsWith('-') && !raw.startsWith('\\')) {
      line += 1;
    }
  }
  out.commentShare = out.addedLines ? Number((out.addedComment / out.addedLines).toFixed(4)) : 0;
  return out;
}

// ---------------------------------------------------------------------------
// The report
// ---------------------------------------------------------------------------

const MOD_RS = 'src-tauri/src/orchestration/mod.rs';

function buildReport(opts) {
  const repoRoot = opts.repoRoot;
  const roots = opts.roots || DEFAULT_ROOTS;
  const legs = (opts.clippyFiles || []).map((f) => {
    try {
      return JSON.parse(fs.readFileSync(f, 'utf8'));
    } catch {
      return null;
    }
  });
  let modRsLines = null;
  try {
    modRsLines = countLines(fs.readFileSync(path.join(repoRoot, MOD_RS), 'utf8')).lines;
  } catch {
    modRsLines = null;
  }
  return {
    schemaVersion: SCHEMA_VERSION,
    generator: 'scripts/code-metrics.cjs',
    commit: opts.commit || null,
    ref: opts.ref || null,
    generatedAt: opts.now || new Date().toISOString(),
    ts: analyzeTypeScript(path.join(repoRoot, 'src'), repoRoot),
    rust: mergeClippy(legs),
    roots: rootTotals(repoRoot, roots),
    modRs: { file: MOD_RS, lines: modRsLines },
    diff: opts.diffText ? analyzeDiff(opts.diffText) : null,
  };
}

function fmt(v) {
  return v === null || v === undefined ? 'n/a' : String(v);
}

function distRow(label, d) {
  const x = d || {};
  return '| ' + label + ' | ' + fmt(x.n) + ' | ' + fmt(x.p50) + ' | ' + fmt(x.p90) + ' | ' + fmt(x.p95) + ' | ' + fmt(x.max) + ' |';
}

function topFunctions(list, key, n) {
  return list
    .filter((f) => typeof f[key] === 'number')
    .sort((a, b) => b[key] - a[key])
    .slice(0, n);
}

function renderSummary(report, topN) {
  const N = topN || 10;
  const L = [];
  L.push('## Code metrics (report-only — nothing here fails the build)');
  L.push('');
  L.push('Commit `' + fmt(report.commit) + '`, schema v' + report.schemaVersion + '.');
  L.push('');
  L.push('### Distributions');
  L.push('');
  L.push('| Metric | n | p50 | p90 | p95 | max |');
  L.push('| --- | --- | --- | --- | --- | --- |');
  L.push(distRow('TS function lines', report.ts.percentiles.fnLines));
  L.push(distRow('TS nesting depth', report.ts.percentiles.depth));
  L.push(distRow('TS argument count', report.ts.percentiles.args));
  L.push(distRow('TS file lines', report.ts.percentiles.fileLines));
  if (report.rust.available) {
    L.push(distRow('Rust function lines', report.rust.percentiles.fnLines));
    L.push(distRow('Rust cognitive complexity', report.rust.percentiles.cognitive));
    L.push(distRow('Rust argument count', report.rust.percentiles.args));
  } else {
    L.push('| Rust (clippy) | n/a | n/a | n/a | n/a | n/a |');
  }
  L.push('');
  if (report.rust.available) {
    L.push('Clippy legs merged: ' + report.rust.platforms.join(', ') + '. Messages read ' + report.rust.messagesSeen + ', parsed ' + report.rust.parsed + '.');
    L.push('');
    L.push('`unwrap` ' + report.rust.totals.unwrap + ' · `expect` ' + report.rust.totals.expect + ' · `panic!` ' + report.rust.totals.panic + ' (product crates, clippy sites).');
    L.push('');
  }
  L.push('`' + report.modRs.file + '`: ' + fmt(report.modRs.lines) + ' lines.');
  L.push('');

  L.push('### Worst functions');
  L.push('');
  L.push('| Language | Function | File | Lines | Extra |');
  L.push('| --- | --- | --- | --- | --- |');
  for (const f of topFunctions(report.ts.functions, 'lines', N)) {
    L.push('| TS | `' + f.name + '` | `' + f.file + ':' + f.line + '` | ' + f.lines + ' | depth ' + f.depth + ', args ' + f.args + ' |');
  }
  for (const f of topFunctions(report.rust.functions, 'lines', N)) {
    L.push('| Rust | `' + f.name + '` | `' + f.file + ':' + f.line + '` | ' + f.lines + ' | cognitive ' + fmt(f.cognitive) + ', args ' + fmt(f.args) + ' |');
  }
  L.push('');

  L.push('### TypeScript import cycles (' + report.ts.cycles.length + ')');
  L.push('');
  if (report.ts.cycles.length === 0) L.push('None.');
  else for (const c of report.ts.cycles) L.push('- ' + c.map((f) => '`' + f + '`').join(' ↔ '));
  L.push('');

  L.push('### Exports with no importer anywhere in the repo (' + report.ts.deadExports.length + ')');
  L.push('');
  L.push('Consumers scanned: `src/`, `test/`, `e2e/`. Decided from import specifiers only, with no type checker — a dynamic `import()`, an entrypoint reached from `index.html`, or a consumer outside those roots reads as dead. Report-only.');
  L.push('');
  const dead = report.ts.deadExports.slice(0, 25);
  for (const d of dead) L.push('- `' + d.file + ':' + d.line + '` — `' + d.name + '`');
  if (report.ts.deadExports.length > dead.length) L.push('- … and ' + (report.ts.deadExports.length - dead.length) + ' more (see the artifact).');
  L.push('');

  L.push('### Roots');
  L.push('');
  L.push('| Root | Files | Lines | Comment | Share | p50 | p90 | p95 | max |');
  L.push('| --- | --- | --- | --- | --- | --- | --- | --- | --- |');
  for (const name of Object.keys(report.roots)) {
    const r = report.roots[name];
    L.push('| `' + name + '` | ' + r.files + ' | ' + r.lines + ' | ' + r.comment + ' | ' + Math.round(r.commentShare * 100) + '% | ' + fmt(r.fileLines.p50) + ' | ' + fmt(r.fileLines.p90) + ' | ' + fmt(r.fileLines.p95) + ' | ' + fmt(r.fileLines.max) + ' |');
  }
  L.push('');
  return L.join('\n') + '\n';
}

// ---------------------------------------------------------------------------
// The delta comment
//
// Every row shows the BASE and the HEAD value, both measured — never a head figure
// with a remembered base beside it (CLAUDE.md, "Every number in a PR body is
// measured at the base AND at the head"). A base this job could not measure prints
// `n/a` and the row says so; it never blocks and never guesses.
//
// This comment is the INSTRUMENT that produces numbers for a reviewer. It is not a
// second place a worker "measures at base and head" — that rule stays on PR-body
// numbers (#2128 part 11).
// ---------------------------------------------------------------------------

function pctCell(base, head, key) {
  const b = base && base[key] !== null && base[key] !== undefined ? base[key] : null;
  const h = head && head[key] !== null && head[key] !== undefined ? head[key] : null;
  if (b === null && h === null) return 'n/a';
  if (b === null) return 'n/a → ' + h;
  if (h === null) return b + ' → n/a';
  const d = h - b;
  return b + ' → ' + h + (d === 0 ? '' : ' (' + (d > 0 ? '+' : '') + d + ')');
}

function distDeltaRow(label, base, head) {
  return (
    '| ' + label + ' |' +
    ' ' + pctCell(base, head, 'n') + ' |' +
    ' ' + pctCell(base, head, 'p50') + ' |' +
    ' ' + pctCell(base, head, 'p90') + ' |' +
    ' ' + pctCell(base, head, 'p95') + ' |' +
    ' ' + pctCell(base, head, 'max') + ' |'
  );
}

function fnKey(f) {
  return f.file + '#' + f.name;
}

// A function is NEW when its file+name pair is absent at the base — the ratchet key
// #2128 part 7 proposes for slice C, computed here in report mode so the false-block
// count exists before any gate is armed.
function newFunctionsOverP95(baseFns, headFns, baseP95, key) {
  if (baseP95 === null || baseP95 === undefined) return [];
  const seen = new Set((baseFns || []).map(fnKey));
  return (headFns || [])
    .filter((f) => typeof f[key] === 'number' && f[key] > baseP95 && !seen.has(fnKey(f)))
    .sort((a, b) => b[key] - a[key]);
}

function buildDelta(base, head, meta) {
  const m = meta || {};
  const L = [];
  const baseTs = (base && base.ts) || { percentiles: {}, functions: [], cycles: [], deadExports: [] };
  const baseRust = (base && base.rust) || { percentiles: {}, functions: [], totals: {}, available: false };
  // An unavailable clippy leg has no population, so its distribution is absent, not
  // zero: `0 -> 2` would read as a measurement nobody made.
  const baseRustPct = baseRust.available ? baseRust.percentiles : {};
  const headRustPct = head.rust.available ? head.rust.percentiles : {};
  const haveBase = !!base;

  L.push(COMMENT_MARKER);
  L.push('## Code metrics — base vs head');
  L.push('');
  L.push(
    'Base `' + fmt(base ? base.commit : m.baseSha) + '` (' + (m.baseSource || (haveBase ? 'measured' : 'unavailable')) + ')' +
      ' · head `' + fmt(head.commit) + '` (measured this run).'
  );
  L.push('');
  L.push('**Every row is report-only.** Nothing here gates the merge; the job cannot turn CI red on a metric. Rows are the reviewer\'s instrument, not a verdict (#2138).');
  L.push('');

  L.push('| Metric | n | p50 | p90 | p95 | max |');
  L.push('| --- | --- | --- | --- | --- | --- |');
  L.push(distDeltaRow('TS function lines', baseTs.percentiles.fnLines, head.ts.percentiles.fnLines));
  L.push(distDeltaRow('TS nesting depth', baseTs.percentiles.depth, head.ts.percentiles.depth));
  L.push(distDeltaRow('TS argument count', baseTs.percentiles.args, head.ts.percentiles.args));
  L.push(distDeltaRow('Rust function lines', baseRustPct.fnLines, headRustPct.fnLines));
  L.push(distDeltaRow('Rust cognitive complexity', baseRustPct.cognitive, headRustPct.cognitive));
  L.push(distDeltaRow('Rust argument count', baseRustPct.args, headRustPct.args));
  L.push('');

  const tsNew = newFunctionsOverP95(baseTs.functions, head.ts.functions, baseTs.percentiles.fnLines && baseTs.percentiles.fnLines.p95, 'lines');
  const rustNew = newFunctionsOverP95(baseRust.functions, head.rust.functions, baseRustPct.fnLines && baseRustPct.fnLines.p95, 'lines');
  L.push('### New functions above the base p95');
  L.push('');
  if (!haveBase) L.push('Base unavailable — not computed.');
  else if (tsNew.length === 0 && rustNew.length === 0) L.push('None.');
  else {
    L.push('| Language | Function | File | Lines | Base p95 |');
    L.push('| --- | --- | --- | --- | --- |');
    for (const f of tsNew.slice(0, 20)) L.push('| TS | `' + f.name + '` | `' + f.file + ':' + f.line + '` | ' + f.lines + ' | ' + fmt(baseTs.percentiles.fnLines.p95) + ' |');
    for (const f of rustNew.slice(0, 20)) L.push('| Rust | `' + f.name + '` | `' + f.file + ':' + f.line + '` | ' + f.lines + ' | ' + fmt(baseRustPct.fnLines.p95) + ' |');
  }
  L.push('');

  const baseCycles = new Set((baseTs.cycles || []).map((c) => c.join(' ↔ ')));
  const newCycles = (head.ts.cycles || []).map((c) => c.join(' ↔ ')).filter((c) => !baseCycles.has(c));
  L.push('### New TypeScript import cycles');
  L.push('');
  if (!haveBase) L.push('Base unavailable — not computed.');
  else if (newCycles.length === 0) L.push('None. (' + (head.ts.cycles || []).length + ' cycles at head, unchanged set.)');
  else for (const c of newCycles) L.push('- ' + c);
  L.push('');

  const d = head.diff;
  L.push('### Added lines');
  L.push('');
  if (!d) L.push('No diff supplied.');
  else {
    L.push('| Row | Value |');
    L.push('| --- | --- |');
    L.push('| Added lines | ' + d.addedLines + ' |');
    L.push('| Comment share of added lines | ' + Math.round(d.commentShare * 100) + '% |');
    L.push('| New `.unwrap()` on added product-Rust lines | ' + d.addedUnwrap.length + ' |');
    L.push('| New `.expect(` on added product-Rust lines | ' + d.addedExpect.length + ' |');
    L.push('| New `panic!(` on added product-Rust lines | ' + d.addedPanic.length + ' |');
    L.push('| New `allow(clippy::…)` on added product-Rust lines | ' + d.addedAllow.length + ' |');
    const sites = d.addedUnwrap.concat(d.addedExpect, d.addedPanic, d.addedAllow).slice(0, 15);
    if (sites.length) {
      L.push('');
      for (const s of sites) L.push('- `' + s.file + ':' + s.line + '` — `' + s.text + '`');
    }
  }
  L.push('');

  const baseMod = base && base.modRs ? base.modRs.lines : null;
  const headMod = head.modRs ? head.modRs.lines : null;
  const modDelta = baseMod !== null && headMod !== null ? headMod - baseMod : null;
  L.push('### Other');
  L.push('');
  L.push('| Row | Base | Head | Delta |');
  L.push('| --- | --- | --- | --- |');
  L.push('| `' + MOD_RS + '` lines | ' + fmt(baseMod) + ' | ' + fmt(headMod) + ' | ' + (modDelta === null ? 'n/a' : (modDelta > 0 ? '+' : '') + modDelta) + ' |');
  L.push('| TS exports with no importer | ' + fmt(haveBase ? (baseTs.deadExports || []).length : null) + ' | ' + (head.ts.deadExports || []).length + ' | ' + (haveBase ? (head.ts.deadExports.length - baseTs.deadExports.length) : 'n/a') + ' |');
  L.push('| Rust `unwrap` sites (clippy) | ' + fmt(baseRust.available ? baseRust.totals.unwrap : null) + ' | ' + fmt(head.rust.available ? head.rust.totals.unwrap : null) + ' | ' + (baseRust.available && head.rust.available ? head.rust.totals.unwrap - baseRust.totals.unwrap : 'n/a') + ' |');
  L.push('');
  if (!haveBase) {
    L.push('> The base could not be measured (' + fmt(m.baseNote) + '). Head figures stand alone; no delta is claimed.');
    L.push('');
  } else if (!baseRust.available) {
    L.push('> Base clippy figures unavailable (' + fmt(m.baseNote || 'the base run kept no clippy artifact') + ') — the Rust rows show `n/a` on the base side rather than a guess.');
    L.push('');
  }
  L.push('<sub>`scripts/code-metrics.cjs` · schema v' + SCHEMA_VERSION + ' · see `doc/design/code-metrics.md`</sub>');
  return L.join('\n') + '\n';
}

// ---------------------------------------------------------------------------
// CLI
// ---------------------------------------------------------------------------

function parseArgs(argv) {
  const out = { _: [], clippy: [] };
  for (let i = 0; i < argv.length; i += 1) {
    const a = argv[i];
    if (!a.startsWith('--')) {
      out._.push(a);
      continue;
    }
    const key = a.slice(2);
    const val = argv[i + 1] !== undefined && !argv[i + 1].startsWith('--') ? argv[(i += 1)] : 'true';
    if (key === 'clippy') out.clippy.push(val);
    else out[key] = val;
  }
  return out;
}

function cmdClippy(args) {
  const acc = newClippyAccumulator(args.platform);
  const rl = readline.createInterface({ input: process.stdin, crlfDelay: Infinity });
  rl.on('line', (line) => {
    const t = line.trim();
    if (!t.startsWith('{')) return;
    let obj;
    try {
      obj = JSON.parse(t);
    } catch {
      return;
    }
    if (obj.reason !== 'compiler-message') return;
    clippyAddMessage(acc, obj.message);
  });
  rl.on('close', () => {
    nameFunctionsFromSource(acc, args['repo-root'] || process.cwd());
    const out = clippyFinish(acc);
    fs.writeFileSync(args.out || 'clippy-metrics.json', JSON.stringify(out) + '\n');
    process.stderr.write(
      'code-metrics: clippy leg ' + out.platform + ' — ' + out.functions.length + ' functions, ' + out.parsed + '/' + out.messagesSeen + ' messages parsed\n'
    );
  });
}

function cmdReport(args) {
  const repoRoot = path.resolve(args['repo-root'] || process.cwd());
  let diffText = null;
  if (args.diff) {
    try {
      diffText = fs.readFileSync(args.diff, 'utf8');
    } catch {
      diffText = null;
    }
  }
  const report = buildReport({
    repoRoot,
    clippyFiles: args.clippy,
    commit: args.commit,
    ref: args.ref,
    diffText,
  });
  fs.writeFileSync(args.out || 'code-metrics.json', JSON.stringify(report, null, 1) + '\n');
  const summary = renderSummary(report);
  if (args.summary) fs.appendFileSync(args.summary, summary);
  else process.stdout.write(summary);
}

function cmdDelta(args) {
  let base = null;
  try {
    base = JSON.parse(fs.readFileSync(args.base, 'utf8'));
  } catch {
    base = null;
  }
  const head = JSON.parse(fs.readFileSync(args.head, 'utf8'));
  const body = buildDelta(base, head, {
    baseSha: args['base-sha'],
    baseSource: args['base-source'],
    baseNote: args['base-note'],
  });
  fs.writeFileSync(args.out || 'code-metrics-comment.md', body);
}

function main(argv) {
  const cmd = argv[0];
  const args = parseArgs(argv.slice(1));
  if (cmd === 'clippy') return cmdClippy(args);
  if (cmd === 'report') return cmdReport(args);
  if (cmd === 'delta') return cmdDelta(args);
  process.stderr.write('usage: code-metrics.cjs <clippy|report|delta> [options]\n');
  process.exitCode = 2;
}

module.exports = {
  SCHEMA_VERSION,
  COMMENT_MARKER,
  percentile,
  distribution,
  countLines,
  analyzeTypeScript,
  stronglyConnected,
  parseClippyText,
  mergeClippy,
  analyzeDiff,
  buildReport,
  renderSummary,
  buildDelta,
  newFunctionsOverP95,
  rootTotals,
  main,
};

if (require.main === module) main(process.argv.slice(2));

#!/usr/bin/env node
'use strict';
// pr-body-check — re-measure every receipt in a POSTED pull-request body against the
// head it claims to describe (#2168 S1, the checker #1842 asked for).
//
// WHY THIS EXISTS. On the ten-PR corpus classified on #2168, 12 of 23 blocking review
// rounds were a NUMBER or a STATEMENT in the body, not a defect in the code, and 7 of
// those were *collateral of the previous round's fix* — the figure went stale when the
// head moved and nothing mechanical pointed at it. Every one of those 12 is a fact a
// machine can re-derive. This re-derives them.
//
// WHAT IT IS NOT. It REFUSES nothing and it exits 0 always, including when it finds a
// mismatch: it is a report a worker reads before `report(done)`, never a gate. A gate on
// prose would have to be right about intent; this is only ever right about a number.
//
// TWO SEVERITIES, and the distinction is the whole contract:
//
//   MISMATCH — a checkable fact DISAGREES with head. The body says 324,776 bytes and the
//              blob is 325,375. There is no reading of the body under which that is fine.
//              On a body that is ready to report, this count must be ZERO.
//   CHECK    — the script narrowed something to a judgment it cannot make: a figure that
//              matches no scope it knows, an identifier it could not find, a line cite
//              whose target it prints for the worker to read. A body may legitimately
//              ship with CHECK rows; each is a sentence to re-read, not a defect.
//
// EVERY BYTE FIGURE CARRIES ITS INSTRUMENT (the #1764 r7/r9 lesson). A file here is CRLF
// on disk and usually LF in the blob, so "3296 bytes" is true of one and false of the
// other, and two review rounds on that PR each swapped one exactly-wrong figure for
// another. So the report never says "the size is N": it prints blob bytes, on-disk bytes,
// blob characters and blob lines side by side, and says which one the body's figure
// matched — or that it matched none.
//
// PURE CORE, INJECTED FACTS. `analyze(body, facts)` does no I/O: `facts` is a plain object
// of everything git/gh would have said (head SHA, diffstat, numstat, per-file blob
// measurements, SHA resolution, run metadata, identifier hit counts). `gather()` builds
// that object from a live PR; the tests build it from a fixture, so the suite never runs
// `gh` and never touches the network (`test/prbodycheck.test.ts`).
//
// Dependency-free CJS. The root package.json is `"type": "module"`, so a `.js` file here
// would be ESM and `require` a ReferenceError.

const fs = require('node:fs');
const { execFileSync } = require('node:child_process');

// Roots an identifier may live under. A backticked identifier appearing in none of these
// is the #1751 r7 shape: the body named `NAME_SITES`, the array is `SITES`.
const GREP_ROOTS = ['src', 'src-tauri', 'crates', 'scripts', 'test', 'e2e', 'doc', 'docs', 'npm', '.github', '.claude', '.orrerix'];

// The one HTML comment a body is allowed to carry: the agent-layer fold marker. Anything
// else is a placeholder somebody meant to fill in (#1758 r1 was a review briefed on a body
// whose RED-EVIDENCE block was still empty).
const ALLOWED_HTML_COMMENTS = new Set(['agent-layer']);

// Textual placeholders that mean "not written yet". Uppercase-only and angle-bracket
// forms: the corpus run showed the lowercase word "placeholder" is ordinary prose in a
// body that DISCUSSES a placeholder finding (#1758's own review response), so matching it
// case-insensitively blocks a known-good body. A `TODO` inside a quoted code line is
// excluded by the fence stripping, not by this list.
const PLACEHOLDER_WORDS = [/\bTBD\b/, /\bTODO\b/, /\bFIXME\b/, /\bXXX\b/, /<PASTE\b/i, /<FILL\b/i, /<PLACEHOLDER\b/i];

// How a SHA's own sentence says what role it plays, read from the ~80 characters BEFORE
// the token; first hit wins, so the order below is the tie-break. `run <id> at <sha>` must
// beat the bare `at` of the head class — telling a head citation from a base one is
// exactly what the #1429 re-stamp failed at, and no `cat-file` or ancestry check can.
//
// Every phrase is ANCHORED to the token (`\s*$` against the preceding text). A loose
// `\bhead\b` anywhere in the window is what an earlier draft used, and it read "measured
// at round-1 head `X`", "the round-4 head blob" and "**Review round 1 (rev-std, head
// `X`)**" as claims about the CURRENT head — eleven false blocks across five known-good
// bodies. A round-qualified head is a DATED section, which `CLAUDE.md` mandates rather
// than forbids, so ROUND_QUALIFIED below takes it back out of the head class.
const SHA_ROLE_PHRASES = [
  ['run-receipt', /\brun\s+\d{6,}\s+(?:at|on|against)\s*`?$/i],
  ['run-receipt', /\b(?:run|workflow)\b[^.]{0,40}\b(?:at|against)\s*`?$/i],
  ['base', /\b(?:cut from|branch point|merge[- ]base|whose parent is|parent is|rebased onto)\s*`?$/i],
  // `head-measured` binds FIGURES to a commit; `head-named` merely names the commit a
  // round happened at. Only the first makes the numbers around it false when it is stale,
  // so only the first is held to resolving on the PR ref.
  ['head-measured', /\b(?:measured at|re-measured at|remeasured at|applies at|as of|dated to)\s*`?$/i],
  // Anchored IMMEDIATELY before the token, so "(rev-std, head `X`)" and "round-1 head `X`"
  // are read while a loose mention of "head" anywhere in the window is not. Both resolve to
  // `head-named` / `dated-head`, neither of which can produce a MISMATCH on its own.
  ['head-named', /\bhead\s*`?$/i],
];

// A head citation carrying one of these is a section dated to an EARLIER head, not a claim
// about the current one.
const ROUND_QUALIFIED = /\b(?:round[- ]?\d|round\s+\w+|previous|earlier|prior|old|pre-rebase|wave-?\d|initial|original|first)\b[^.]{0,30}$/i;

// ---------------------------------------------------------------------------
// Text shaping: fences, and what each class of check is allowed to read.
// ---------------------------------------------------------------------------

// Split the body into lines tagged with whether they sit inside a ``` fence.
//
// Receipts quote commands and their output, and that output legitimately carries figures
// that do NOT describe head — a scratch round's numstat, a panic line, a diffstat printed
// as an example of the wrong instrument. So the figure checks read prose only. The SHA and
// run-id checks read everything, because a stale SHA is stale wherever it is printed.
//
// Residual, stated here because it is the checker's own blind spot: an identifier named
// only inside a fence is not checked. Fenced content is quoted machine output rather than
// a claim about naming, and the corpus bears that out — every identifier defect in the
// #2168 classification (#1751 r7 `NAME_SITES`) was in prose.
function tagLines(body) {
  const out = [];
  let inFence = false;
  let fence = '';
  const rows = String(body).replace(/\r\n/g, '\n').split('\n');
  for (let i = 0; i < rows.length; i += 1) {
    const raw = rows[i];
    const m = raw.match(/^\s*(`{3,}|~{3,})/);
    if (m) {
      if (!inFence) { inFence = true; fence = m[1][0]; out.push({ n: i + 1, text: raw, fence: true }); continue; }
      if (m[1][0] === fence) { inFence = false; out.push({ n: i + 1, text: raw, fence: true }); continue; }
    }
    out.push({ n: i + 1, text: raw, fence: inFence });
  }
  return out;
}

const proseLines = (lines) => lines.filter((l) => !l.fence);

function num(s) { return Number(String(s).replace(/[,_\s]/g, '')); }
function fmt(n) { return typeof n === 'number' && isFinite(n) ? n.toLocaleString('en-US') : String(n); }
function short(sha) { return String(sha || '').slice(0, 8); }

// ---------------------------------------------------------------------------
// Extraction — every shape a body states a receipt in.
// ---------------------------------------------------------------------------

const RE = {
  // `12 files changed, 2,959 insertions(+), 41 deletions(-)` — git's own summary line.
  diffstatCanonical: /(\d[\d,]*)\s+files?\s+changed,\s*(\d[\d,]*)\s+insertions?\(\+\)(?:,\s*(\d[\d,]*)\s+deletions?\(-\))?/g,
  // `4 files, **773 insertions, 27 deletions**` — the prose form bodies actually use.
  diffstatProse: /(\d[\d,]*)\s+files?\b[^.\n]{0,40}?[*`]{0,2}(\d[\d,]*)[*`]{0,2}\s+insertions?\b(?:[^.\n]{0,30}?[*`]{0,2}(\d[\d,]*)[*`]{0,2}\s+deletions?)?/g,
  // A bare `N insertions` / `N deletions` with no file count beside it.
  // A figure in a body is wrapped in bold or in a code span about as often as it is bare,
  // and the two mix inside one sentence: "`9 + 6` insertions, `0` deletions" (#2105 r2) is
  // invisible to a class that admits asterisks only.
  insertions: /[*`]{0,2}(\d[\d,]*)[*`]{0,2}\s+insertions?\b/g,
  deletions: /[*`]{0,2}(\d[\d,]*)[*`]{0,2}\s+deletions?\b/g,
  // `325,375 bytes`, `100 chars`, `4,211 lines`.
  byteFigure: /[*`]{0,2}(\d[\d,]*)[*`]{0,2}[- ]?(bytes?|chars?|characters?|lines?)\b/gi,
  // Any hex token 7..40 long: a commit SHA or a blob hash.
  hex: /\b([0-9a-f]{7,40})\b/g,
  // Run ids: 9-12 digits, bare or inside an actions URL.
  runUrl: /actions\/runs\/(\d{6,})/g,
  runBare: /\b(\d{9,12})\b/g,
  backtick: /`([^`\n]+)`/g,
};

// A backticked token that is plausibly a code identifier this repo would contain.
//
// Deliberately narrow: it must carry `_`, `::` or a `()` call suffix. A bare CamelCase or
// lowercase word is NOT enough — bodies are full of prose nouns in backticks (`main`,
// `HEAD`, `gh`, `blob`) and a checker that flags those produces a list nobody reads. That
// narrowness is the residual: `SITES` spelled as `Sites` would pass unchecked.
function isIdentifierToken(tok) {
  const t = String(tok).trim();
  if (t.length < 4 || t.length > 120) return false;
  if (/\s/.test(t)) return false;
  if (/^[0-9a-f]{7,40}$/.test(t)) return false;              // a SHA, handled elsewhere
  if (/^\d[\d,._]*$/.test(t)) return false;                  // a number
  if (/[/\\]/.test(t)) return false;                         // a path, handled elsewhere
  if (/^--?[A-Za-z]/.test(t)) return false;                  // a CLI flag
  const core = t.replace(/\(\)$/, '');
  if (!/^[A-Za-z_][A-Za-z0-9_]*(?:::[A-Za-z_][A-Za-z0-9_]*)*$/.test(core)) return false;
  return /_/.test(core) || /::/.test(core) || /\(\)$/.test(t);
}

// An HTML comment whose TEXT tells somebody to fill it in is a placeholder however it is
// named — `<!-- RED-EVIDENCE: paste the failing test output here -->` is the #1758 r1 shape,
// and it carries none of the bare markers above.
const COMMENT_FILL_WORD = /\b(?:paste|fill|insert|replace|add|write|here)\b/i;

// A line whose diffstat describes something narrower than the PR.
const SCOPED_DIFFSTAT = /\bgit show\b|\bthis commit\b|\bper[- ]commit\b|\bround[- ]?\d|\bscratch\b|\bfinding\b|\bthe delta\b|\bslice\b/i;

// What must sit just before a bare 9-12 digit number for it to be read as a run id. `job`
// is deliberately NOT here: a job id is the same shape and `gh run view` cannot resolve
// one, so admitting it reports five known-good bodies as citing runs that do not exist.
const RE_RUN_CONTEXT = /\b(?:run|runs|workflow)\b[^A-Za-z0-9]{0,12}\*{0,2}$/i;

// `path/to/file.ts:1234` — a line cite. #1764 r3 and r5 were both a cite that had drifted
// off its anchor. The script cannot know the intent, so it PRINTS the cited line at head.
const RE_LINECITE = /`?([\w./-]+\.(?:rs|ts|js|cjs|mjs|md|yml|yaml|toml|json|html|css|ps1|sh))`?:(\d+)\b/g;

function extract(body) {
  const lines = tagLines(body);
  const prose = proseLines(lines);
  const claims = {
    diffstats: [], insertions: [], deletions: [], byteFigures: [], shas: [],
    runs: [], identifiers: [], lineCites: [], placeholders: [], htmlComments: [],
    quantities: [],
  };

  const spans = [];   // regions already consumed by a diffstat match, so the loose
                      // `N insertions` pass does not double-report them
  for (const l of prose) {
    for (const m of l.text.matchAll(RE.diffstatCanonical)) {
      claims.diffstats.push({ line: l.n, text: l.text.trim(), form: 'canonical', files: num(m[1]), insertions: num(m[2]), deletions: m[3] === undefined ? null : num(m[3]) });
      spans.push([l.n, m.index, m.index + m[0].length]);
    }
    for (const m of l.text.matchAll(RE.diffstatProse)) {
      if (spans.some(([n, a, b]) => n === l.n && m.index >= a && m.index < b)) continue;
      claims.diffstats.push({ line: l.n, text: l.text.trim(), form: 'prose', files: num(m[1]), insertions: num(m[2]), deletions: m[3] === undefined ? null : num(m[3]) });
      spans.push([l.n, m.index, m.index + m[0].length]);
    }
    const covered = (i) => spans.some(([n, a, b]) => n === l.n && i >= a && i < b);
    for (const m of l.text.matchAll(RE.insertions)) if (!covered(m.index)) claims.insertions.push({ line: l.n, text: l.text.trim(), value: num(m[1]) });
    for (const m of l.text.matchAll(RE.deletions)) if (!covered(m.index)) claims.deletions.push({ line: l.n, text: l.text.trim(), value: num(m[1]) });

    for (const m of l.text.matchAll(RE.byteFigure)) {
      const unit = m[2].toLowerCase().replace(/s$/, '').replace('character', 'char');
      claims.byteFigures.push({ line: l.n, index: m.index, text: l.text.trim(), value: num(m[1]), unit, paths: pathsOn(l.text) });
    }

    for (const m of l.text.matchAll(RE.backtick)) {
      if (isIdentifierToken(m[1])) claims.identifiers.push({ line: l.n, token: m[1].trim().replace(/\(\)$/, ''), text: l.text.trim() });
    }

    for (const re of PLACEHOLDER_WORDS) {
      const m = l.text.match(re);
      if (m) claims.placeholders.push({ line: l.n, marker: m[0], text: l.text.trim() });
    }
  }

  // Line cites, SHAs and run ids are read from the WHOLE body: a stale SHA inside a fenced
  // receipt is exactly as stale as one in prose.
  for (const l of lines) {
    for (const m of l.text.matchAll(RE_LINECITE)) claims.lineCites.push({ line: l.n, path: m[1], cited: Number(m[2]), fence: l.fence, text: l.text.trim() });
    const seenRun = new Set();
    for (const m of l.text.matchAll(RE.runUrl)) { seenRun.add(m[1]); claims.runs.push({ line: l.n, index: m.index, id: m[1], fence: l.fence, text: l.text.trim() }); }
    for (const m of l.text.matchAll(RE.runBare)) {
      if (seenRun.has(m[1])) continue;
      // A bare 9-12 digit number is a run id only where its own sentence says so. Without
      // this the corpus yields GitHub comment ids and audit figures as "runs that do not
      // exist" — five false blocks on #1758 alone.
      if (!RE_RUN_CONTEXT.test(l.text.slice(Math.max(0, m.index - 40), m.index))) continue;
      seenRun.add(m[1]);
      claims.runs.push({ line: l.n, index: m.index, id: m[1], fence: l.fence, text: l.text.trim() });
    }
    for (const m of l.text.matchAll(RE.hex)) {
      if (/^\d+$/.test(m[1])) continue;                      // all digits: a number, not a SHA
      const before = l.text.slice(Math.max(0, m.index - 80), m.index);
      claims.shas.push({ line: l.n, index: m.index, sha: m[1], role: classifySha(before), fence: l.fence, before: before.trim(), text: l.text.trim() });
    }
  }

  // HTML comments come from the whole body: one inside a fence is still a placeholder
  // somebody left behind.
  const flat = String(body).replace(/\r\n/g, '\n');
  for (const m of flat.matchAll(/<!--([\s\S]*?)-->/g)) {
    const name = m[1].trim();
    if (ALLOWED_HTML_COMMENTS.has(name)) continue;
    claims.htmlComments.push({ line: flat.slice(0, m.index).split('\n').length, name: name.slice(0, 80), text: m[0].replace(/\s+/g, ' ').slice(0, 120) });
  }

  claims.quantities = groupQuantities(prose);
  return claims;
}

// File extensions a token must carry to be read as a path. Without this, `Buffer.length`
// and `assert_eq!` read as filenames and the script asks git for paths that cannot exist.
const PATH_EXT = /\.(?:rs|ts|tsx|js|cjs|mjs|md|markdown|yml|yaml|toml|json|jsonl|html|css|ps1|sh|lock|txt)$/i;

// Which repo-looking paths a line names. A byte figure with no subject is not checked at
// all — naming the subject is what makes the four-instrument table meaningful.
function pathsOn(text) {
  const out = [];
  for (const m of String(text).matchAll(/`([\w./-]+\.[A-Za-z0-9]{1,8})`/g)) if (PATH_EXT.test(m[1])) out.push(m[1]);
  for (const m of String(text).matchAll(/\b((?:src|src-tauri|crates|scripts|test|e2e|doc|docs|npm|\.github|\.claude|\.orrerix)\/[\w./-]+)\b/g)) if (PATH_EXT.test(m[1])) out.push(m[1]);
  return [...new Set(out)];
}

function classifySha(before) {
  for (let i = 0; i < SHA_ROLE_PHRASES.length; i += 1) {
    if (!SHA_ROLE_PHRASES[i][1].test(before)) continue;
    const role = SHA_ROLE_PHRASES[i][0];
    if (role.indexOf('head') === 0 && ROUND_QUALIFIED.test(before)) return 'dated-head';
    return role;
  }
  return 'unclassified';
}

// One quantity stated twice with two values (spec (e)).
//
// The key is the UNIT PHRASE following the number — two or three lowercase words — so
// `17 surviving sites` and `14 surviving sites` collide while `4 files` and
// `773 insertions` do not. A one-word key is far too coarse: `3 rounds` and `7 rounds`
// are routinely different scopes, and a checker that says so on every body is noise.
// Two words is therefore the floor, and it is also the residual: a pair stated with
// different wording ("52 ambiguous" / "83 collapsed", #2139 r5) does not collide here.
function groupQuantities(prose) {
  const by = new Map();
  for (const l of prose) {
    const plain = l.text.replace(/[*`_]/g, '');
    // EXACTLY two words, never a greedy run: "17 surviving sites" and "14 surviving sites
    // remain" would otherwise key on phrases of different lengths and never collide, which
    // is the one thing this check exists to notice.
    for (const m of plain.matchAll(/\b(\d[\d,]*)\s+([a-z][a-z-]{2,}\s+[a-z][a-z-]{2,})\b/g)) {
      const key = m[2].toLowerCase().replace(/\s+/g, ' ');
      if (!by.has(key)) by.set(key, []);
      by.get(key).push({ line: l.n, value: num(m[1]), text: l.text.trim() });
    }
  }
  const out = [];
  for (const [key, hits] of by) {
    const values = [...new Set(hits.map((h) => h.value))];
    if (values.length > 1) out.push({ key, values, hits });
  }
  return out;
}

// ---------------------------------------------------------------------------
// Analysis — the pure core. `facts` is everything git/gh would have said.
//
// A fact that is ABSENT is never a MISMATCH: the script says it could not check, which is
// why a fixture may cover one axis without silently blessing the rest.
// ---------------------------------------------------------------------------

function analyze(body, facts) {
  const f = facts || {};
  const findings = [];
  const add = (severity, check, line, message, detail) => findings.push({ severity, check, line, message, detail: detail || null });
  const claims = extract(body);

  // (a) diffstat at head.
  const ds = f.diffstat;
  for (const d of claims.diffstats) {
    if (!ds) { add('CHECK', 'diffstat', d.line, `diffstat "${fmt(d.files)} files / ${fmt(d.insertions)}+ / ${d.deletions === null ? '?' : fmt(d.deletions)}-" not checked: no diffstat was measured`, d.text); continue; }
    const ok = d.files === ds.files && d.insertions === ds.insertions && (d.deletions === null || d.deletions === ds.deletions);
    if (ok) continue;
    // A diffstat whose own line names a narrower subject — `git show --stat <sha>`, one
    // commit, one round, one file — is not a claim about the PR, and holding it to the
    // PR's numbers blocks a body that is discussing a per-commit figure on purpose.
    const scoped = SCOPED_DIFFSTAT.test(d.text);
    add(scoped ? 'CHECK' : 'MISMATCH', 'diffstat', d.line,
      `body says ${fmt(d.files)} files / ${fmt(d.insertions)}+ / ${d.deletions === null ? '—' : fmt(d.deletions)}-; `
      + `git diff ${short(f.mergeBase)}..${short(f.head)} --numstat says ${fmt(ds.files)} files / ${fmt(ds.insertions)}+ / ${fmt(ds.deletions)}-`
      + (scoped ? ' — the line names a narrower subject, so say which range it measures' : ''),
      d.text);
  }
  if (ds) {
    const okIns = new Set([ds.insertions].concat(Object.keys(f.numstat || {}).map((k) => f.numstat[k].insertions)));
    for (const i of claims.insertions) if (!okIns.has(i.value)) add('CHECK', 'insertions', i.line, `"${fmt(i.value)} insertions" is neither the head total (${fmt(ds.insertions)}) nor any per-file count at head`, i.text);
    const okDel = new Set([ds.deletions].concat(Object.keys(f.numstat || {}).map((k) => f.numstat[k].deletions)));
    for (const d of claims.deletions) if (!okDel.has(d.value)) add('CHECK', 'deletions', d.line, `"${fmt(d.value)} deletions" is neither the head total (${fmt(ds.deletions)}) nor any per-file count at head`, d.text);
  }

  // (a cont.) byte / char / line figures, WITH THE INSTRUMENT NAMED.
  //
  // The subject is chosen in the order that makes the check EXACT before it makes it
  // broad, because a byte figure in a body routinely describes something other than the
  // file at head — the BASE blob of an append proof, the blob of an earlier round:
  //
  //   1. the nearest BLOB HASH to the left on the same line. A blob hash is the one
  //      citation a rebase cannot invalidate (CLAUDE.md's #1470 B1 bullet), and `git
  //      cat-file -s` settles the figure outright. `base 728f7407 300,527 bytes -> head
  //      61855f9c 325,375 bytes` resolves to two different blobs and both check clean;
  //      the round-3 defect on #2140 was 324,776 written beside the head blob, and against
  //      that blob it is a MISMATCH.
  //   2. failing that, a line naming exactly ONE measured path and carrying exactly ONE
  //      figure of that unit — then head's four instruments decide.
  //   3. otherwise the script reports the instrument table and lets the worker decide. A
  //      figure whose subject is ambiguous is a CHECK, never a MISMATCH.
  const figuresOnLine = new Map();
  for (const b of claims.byteFigures) figuresOnLine.set(b.line, (figuresOnLine.get(b.line) || 0) + 1);
  for (const b of claims.byteFigures) {
    const blobTok = claims.shas
      .filter((s) => s.line === b.line && s.index < b.index && (f.blobs || {})[s.sha])
      .sort((x, y) => y.index - x.index)[0];
    if (blobTok) {
      const bl = f.blobs[blobTok.sha];
      const got = b.unit === 'char' ? bl.chars : b.unit === 'line' ? bl.lines : bl.bytes;
      if (b.value !== got) {
        add('MISMATCH', 'byte-figure', b.line,
          `"${fmt(b.value)} ${b.unit}s" is stated for blob \`${blobTok.sha}\`, which is ${fmt(bl.bytes)} bytes / ${fmt(bl.chars)} chars / ${fmt(bl.lines)} lines (git cat-file -s)`, b.text);
      }
      continue;
    }
    const measured = b.paths.filter((p) => (f.files || {})[p]);
    if (measured.length !== 1) continue;
    const m = f.files[measured[0]];
    const table = `${measured[0]} at head: blob ${fmt(m.blobBytes)} bytes / on-disk ${m.diskBytes === null || m.diskBytes === undefined ? 'n/a' : fmt(m.diskBytes)} bytes / ${fmt(m.blobChars)} chars / ${fmt(m.blobLines)} lines (blob ${short(m.blob)})`;
    const matched = [];
    if (b.value === m.blobBytes) matched.push('blob bytes');
    if (b.value === m.diskBytes) matched.push('on-disk bytes');
    if (b.value === m.blobChars) matched.push('blob chars');
    if (b.value === m.blobLines) matched.push('blob lines');
    const wanted = b.unit === 'char' ? ['blob chars'] : b.unit === 'line' ? ['blob lines'] : ['blob bytes', 'on-disk bytes'];
    if (!matched.length) {
      // Only a line stating ONE figure about ONE file can be held to head; anything else
      // legitimately names a base blob, a delta or another round.
      if (figuresOnLine.get(b.line) === 1) add('MISMATCH', 'byte-figure', b.line, `"${fmt(b.value)} ${b.unit}s" matches no measurement of ${table}`, b.text);
      else add('CHECK', 'byte-figure', b.line, `"${fmt(b.value)} ${b.unit}s" matches no measurement of ${table} — the line states several figures, so say which subject each names`, b.text);
    } else if (!matched.some((k) => wanted.indexOf(k) !== -1)) {
      add('CHECK', 'byte-figure', b.line, `"${fmt(b.value)} ${b.unit}s" is right for a DIFFERENT instrument (${matched.join(', ')}) — name the instrument. ${table}`, b.text);
    }
  }

  // Blob hashes cited for a file. A body dates a count to a blob precisely because a blob
  // survives the rebase that invalidates every SHA beside it — so a blob that is not that
  // file's blob at head is a measurement taken at an earlier round (#2139 r2 cited
  // `b14efe7e` after the file had moved to `89a7f7c9`). CHECK rather than MISMATCH: an
  // append proof legitimately names the BASE blob, and the script cannot read which.
  for (const s of claims.shas) {
    const info = (f.shaInfo || {})[s.sha];
    if (!info || !info.resolves || info.type !== 'blob') continue;
    const named = pathsOn(s.text).filter((p) => (f.files || {})[p]);
    if (named.length !== 1) continue;
    const m = f.files[named[0]];
    if (String(m.blob).startsWith(s.sha)) continue;
    add('CHECK', 'blob', s.line, `blob \`${s.sha}\` is cited beside ${named[0]}, whose blob at head is ${short(m.blob)} — an earlier round's blob, or the base blob of an append proof? Say which.`, s.text);
  }

  // per-file counts: a line naming exactly one measured path, plus figures none of which
  // is that file's insertions, deletions or their sum. Skipped on a line that is about
  // SIZE rather than about a diff — a byte figure or a blob hash there means the numbers
  // are answering a different question.
  for (const l of proseLines(tagLines(body))) {
    if (/\b(?:bytes?|chars?|characters?|blob)\b/i.test(l.text)) continue;
    const named = pathsOn(l.text).filter((p) => (f.numstat || {})[p]);
    if (named.length !== 1) continue;
    const st = f.numstat[named[0]];
    const nums = [];
    for (const m of l.text.matchAll(/\b(\d[\d,]*)\b/g)) { const v = num(m[1]); if (v > 0) nums.push(v); }
    if (!nums.length) continue;
    const ok = new Set([st.insertions, st.deletions, st.insertions + st.deletions]);
    if (!nums.some((n) => ok.has(n))) add('CHECK', 'per-file', l.n, `line names ${named[0]} with figures ${nums.map(fmt).join(', ')}; at head that file is +${fmt(st.insertions)} / -${fmt(st.deletions)}`, l.text.trim());
  }

  // (b) SHAs: resolvable, on the PR ref, and playing the role their sentence assigns.
  //
  // A hex token in one of these bodies is as often a BLOB hash as a commit — a blob is
  // what a body cites when it dates a count to something a rebase cannot move — so the
  // object TYPE is resolved before anything else, and a blob is never asked about
  // ancestry. An unresolvable token is a MISMATCH only where its own sentence assigns it
  // a role; an unclassified one is a CHECK, because seven-plus letters of [a-f] is also
  // how English spells a few real words.
  for (const s of claims.shas) {
    const info = (f.shaInfo || {})[s.sha];
    if (!info) { add('CHECK', 'sha', s.line, `\`${s.sha}\` (role read as: ${s.role}) was not resolved`, s.text); continue; }
    if (!info.resolves) {
      // Only a SHA whose own sentence gives it a role is held to resolving: seven or more
      // letters of [a-f0-9] is also how a log line, another project's commit, or the odd
      // English word is spelled, and none of those is this PR's problem.
      if (s.role === 'unclassified') add('CHECK', 'sha', s.line, `\`${s.sha}\` resolves to no object — if it is a SHA, nobody can check it`, s.text);
      else add('MISMATCH', 'sha', s.line, `\`${s.sha}\` is cited as ${s.role} but resolves to no object — a SHA nobody can check`, s.text);
      continue;
    }
    if (info.type && info.type !== 'commit') continue;         // a blob/tree; the byte-figure check owns it
    if (s.role.indexOf('head') !== -1 && f.head && !String(f.head).startsWith(s.sha)) {
      // Spec (f). A head citation that is NOT the current head is a MISMATCH only when the
      // figures it BINDS are unverifiable — an orphaned commit under `measured at`. A head
      // still on the PR ref is a section dated to an earlier head, which CLAUDE.md's
      // per-section dating rule mandates rather than forbids, and a `head-named` /
      // `dated-head` citation binds no figure at all. Both are reported; the severity is
      // the difference between "this is wrong" and "the head moved under this section".
      if (!info.onPrRef && s.role === 'head-measured') add('MISMATCH', 'sha', s.line, `\`${s.sha}\` is cited as ${s.role} ("...${s.before.slice(-42)}") but head is ${short(f.head)} and it is not on refs/pull/${f.pr}/head — orphaned, so nothing it dates can be re-derived. Subject: "${info.subject || '?'}"`, s.text);
      else add('CHECK', 'sha', s.line, `\`${s.sha}\` is cited as ${s.role} ("...${s.before.slice(-42)}") but head is now ${short(f.head)}${info.onPrRef ? '' : ' and it is NOT on the PR ref'} — re-read every figure this section carries. Subject: "${info.subject || '?'}"`, s.text);
      continue;
    }
    if (!info.onPrRef && s.role === 'base') {
      add('MISMATCH', 'sha', s.line, `\`${s.sha}\` is cited as ${s.role} but is not an ancestor of refs/pull/${f.pr}/head — orphaned by a rebase. Subject: "${info.subject || '?'}"`, s.text);
    } else if (!info.onPrRef && !s.fence) {
      add('CHECK', 'sha', s.line, `\`${s.sha}\` resolves but is not on refs/pull/${f.pr}/head (subject: "${info.subject || '?'}") — legitimate for a scratch round or a sibling PR, so say which`, s.text);
    }
  }

  // (c) run ids against `gh run view --json headSha,conclusion`.
  for (const r of claims.runs) {
    const info = (f.runs || {})[r.id];
    if (!info) { add('CHECK', 'run', r.line, `run ${r.id} was not resolved (\`gh run view ${r.id} --json headSha,conclusion\`)`, r.text); continue; }
    if (!info.exists) { add('MISMATCH', 'run', r.line, `run ${r.id} does not exist`, r.text); continue; }
    // Pairing a run with a SHA takes two passes, because one LINE routinely carries several
    // rounds — a markdown row with a run and a head per column, a paragraph walking three
    // pushes. Pairing pairwise manufactured fourteen false blocks on #2104; pairing with
    // the nearest SHA manufactured five more, because the nearest is often a commit the
    // sentence is CONTRASTING the run with ("the first red (run X, superseded) ... `Y`
    // restructures both tests").
    //   1. If any SHA on the line IS the run's head, the line is right. Nothing to say.
    //   2. Otherwise a SHA BOUND to the run — within 40 characters, with no sentence or
    //      cell boundary between them — is a claim about this run, and a wrong one.
    //   3. A line that names no SHA near the run makes no claim to check.
    const onLine = claims.shas.filter((s) => s.line === r.line);
    if (!onLine.some((s) => String(info.headSha || '').startsWith(s.sha))) {
      const bound = onLine.filter((s) => {
        if (Math.abs(s.index - r.index) > 40) return false;
        const a = Math.min(s.index, r.index); const b = Math.max(s.index, r.index);
        return !/[.;|—]/.test(r.text.slice(a, b));
      })[0];
      if (bound) add('MISMATCH', 'run', r.line, `run ${r.id} ran at ${short(info.headSha)}, but the SHA bound to it on the line is \`${bound.sha}\``, r.text);
    }
    // Conclusion is a CHECK, never a MISMATCH: a body legitimately narrates a red run it
    // is adjudicating ("the rerun of the failed job finished success"), and no keyword
    // test separates that from a false green claim. The row still says what the run did.
    // `0 failed` is a green receipt, so a failure word a count precedes is stripped first.
    const saysGreen = /\bgreen\b|\bpassed\b|\bsuccess\b|\ball three platforms\b/i.test(r.text);
    const saysRed = /\b(?:red|failed|failing|failure)\b/i.test(r.text.replace(/\b(?:0|no|zero)\s+(?:failed|failures?)\b/gi, ''));
    if (saysGreen && info.conclusion !== 'success') add('CHECK', 'run', r.line, `run ${r.id} concluded "${info.conclusion}" (attempt ${info.runAttempt}); the line reads as green`, r.text);
    if (saysRed && info.conclusion === 'success') add('CHECK', 'run', r.line, `run ${r.id} concluded "success" (attempt ${info.runAttempt}) — a rerun clears a failure without moving the SHA`, r.text);
  }

  // (d) every backticked identifier greps to a hit under the repo roots.
  // A CHECK rather than a MISMATCH, and the corpus is the argument: `base_ref_changed` and
  // `base_ref_deleted` on #1751 are GitHub timeline-event names, correct and by design
  // absent from this repo. "This string names nothing here" has a legitimate reading the
  // script cannot rule out (an external API field, another project's symbol), so it says
  // so rather than calling it wrong. The row is still the one that catches a `NAME_SITES`
  // written for a `SITES` (#1751 r7).
  for (const id of claims.identifiers) {
    const hits = (f.identifierHits || {})[id.token];
    if (hits === undefined) continue;
    if (hits === 0) add('CHECK', 'identifier', id.line, `\`${id.token}\` names nothing under ${GREP_ROOTS.join(' ')} at head — a repo symbol misspelled, or a name from somewhere else?`, id.text);
  }

  // line cites — print what the cite points at, so the worker reads it rather than
  // trusting a number derived at a commit that has since moved.
  for (const c of claims.lineCites) {
    const m = (f.files || {})[c.path];
    if (!m) continue;
    if (c.cited > m.blobLines) { add('MISMATCH', 'line-cite', c.line, `${c.path}:${c.cited} is past the end of that file at head (${fmt(m.blobLines)} lines)`, c.text); continue; }
    if (m.lineAt && m.lineAt[c.cited] !== undefined) add('CHECK', 'line-cite', c.line, `${c.path}:${c.cited} at head reads: ${JSON.stringify(String(m.lineAt[c.cited]).trim().slice(0, 120))}`, c.text);
  }

  // (e) one quantity stated twice with two values.
  for (const q of claims.quantities) {
    add('CHECK', 'quantity', q.hits[0].line, `"${q.key}" is stated with ${q.values.length} different values: ${q.values.map(fmt).join(', ')} (lines ${q.hits.map((h) => h.line).join(', ')})`, q.hits.map((h) => `L${h.line}: ${h.text}`).join('\n'));
  }

  // (g) placeholders still in the body.
  for (const p of claims.placeholders) add('MISMATCH', 'placeholder', p.line, `placeholder "${p.marker}" is still in the body`, p.text);
  // An EMPTY comment, or one whose text is a fill-me marker, is a placeholder. A named one
  // may be a live anchor — `<!-- code-metrics -->` on #2139 is the marker the CI job posts
  // its report under, and the body says so — so that is a CHECK, not a refusal.
  for (const c of claims.htmlComments) {
    const empty = /^[-_\s]*$/.test(c.name) || PLACEHOLDER_WORDS.some((re) => re.test(c.name)) || COMMENT_FILL_WORD.test(c.name);
    add(empty ? 'MISMATCH' : 'CHECK', 'placeholder', c.line, `HTML comment left in the body: ${c.text}${empty ? '' : ' — an anchor, or something left unfilled?'}`, null);
  }

  findings.sort((a, b) => (a.line - b.line) || a.check.localeCompare(b.check) || a.message.localeCompare(b.message));
  // A token repeated on one line yields one finding, not one per occurrence: the row a
  // worker acts on is the sentence, and a duplicated row reads as two defects.
  const seen = new Set();
  const deduped = findings.filter((x) => {
    const k = `${x.line} ${x.check} ${x.message}`;
    if (seen.has(k)) return false;
    seen.add(k);
    return true;
  });
  findings.length = 0;
  findings.push(...deduped);
  return {
    findings,
    counts: {
      MISMATCH: findings.filter((x) => x.severity === 'MISMATCH').length,
      CHECK: findings.filter((x) => x.severity === 'CHECK').length,
    },
    claims: {
      diffstats: claims.diffstats.length,
      byte_figures: claims.byteFigures.length,
      shas: new Set(claims.shas.map((s) => s.sha)).size,
      runs: new Set(claims.runs.map((r) => r.id)).size,
      identifiers: new Set(claims.identifiers.map((i) => i.token)).size,
      line_cites: claims.lineCites.length,
      quantity_groups: claims.quantities.length,
    },
  };
}

// ---------------------------------------------------------------------------
// (h) --list-claims: the shapes S2's twin sweep has to re-derive by hand.
// ---------------------------------------------------------------------------

const CLAIM_SHAPES = [
  ['ordinal', /\bthe\s+(first|second|third|fourth|fifth|sixth|seventh|eighth|ninth|tenth|last|only)\b/gi],
  ['count', /\b(\d+|one|two|three|four|five|six|seven|eight|nine|ten)\s+(sites?|places?|callers?|instances?|occurrences?|surfaces?|readers?|writers?|arms?|branches?|tests?|files?)\b/gi],
  ['absolute', /\b(only|never|no other|nothing else|nowhere else|every|always|exactly one|the sole|no longer|none of)\b/gi],
];

const PROSE_FILE = /\.(?:md|markdown)$/i;
const COMMENT_LINE = /^\s*(?:\/\/|\/\*|\*|#|<!--)/;

// `addedProse` is [{ file, text }] — the ADDED lines of the diff, restricted to prose:
// markdown, and comment lines in source. An ordinal in added code is a variable name; an
// ordinal in added prose is a claim about a list the diff itself may have just grown
// (#2140 B1: adding a fourth clear site bumped "the third" to "the fourth" in step, onto
// the wrong site, against three other surfaces that still said otherwise).
function listClaims(addedProse) {
  const rows = [];
  for (const row of addedProse || []) {
    for (const shape of CLAIM_SHAPES) {
      for (const m of String(row.text).matchAll(shape[1])) rows.push({ file: row.file, kind: shape[0], match: m[0], text: String(row.text).trim().slice(0, 200) });
    }
  }
  return rows;
}

function addedProseFromDiff(diffText) {
  const out = [];
  let file = null;
  for (const raw of String(diffText).replace(/\r\n/g, '\n').split('\n')) {
    const m = raw.match(/^\+\+\+ b\/(.+)$/);
    if (m) { file = m[1]; continue; }
    if (!file || raw.charAt(0) !== '+' || raw.startsWith('+++')) continue;
    const text = raw.slice(1);
    if (PROSE_FILE.test(file) || COMMENT_LINE.test(text)) out.push({ file, text });
  }
  return out;
}

// ---------------------------------------------------------------------------
// I/O — building `facts` from a live PR. Nothing below this line is reached by the tests.
// ---------------------------------------------------------------------------

// `stdio` pipes stderr rather than inheriting it: most of the git calls below are probes
// whose failure is the answer ("this token is not an object"), and a probe that prints
// `fatal:` to the terminal reads as a broken tool.
function sh(cmd, args, opts) {
  return execFileSync(cmd, args, Object.assign({ encoding: 'utf8', maxBuffer: 256 * 1024 * 1024, stdio: ['ignore', 'pipe', 'pipe'] }, opts || {}));
}
function shq(cmd, args, opts) {
  try { return { ok: true, out: sh(cmd, args, opts) }; }
  catch (e) { return { ok: false, out: (e && e.stdout) || '', err: String((e && e.stderr) || (e && e.message) || '') }; }
}

function gather(pr, opts) {
  const o = opts || {};
  const repoArgs = o.repo ? ['--repo', o.repo] : [];
  const meta = JSON.parse(sh('gh', ['pr', 'view', String(pr), ...repoArgs, '--json', 'body,headRefOid,baseRefName,number']));
  const head = meta.headRefOid;
  const body = meta.body || '';
  const baseBranch = o.baseBranch || meta.baseRefName || 'main';

  // The leading + is load-bearing: the PR ref moves non-fast-forward on every force-push,
  // which is exactly the case this check exists for (ci-validate SKILL.md).
  const ref = `refs/tmp/prbc${pr}`;
  shq('git', ['fetch', 'origin', `+refs/pull/${pr}/head:${ref}`]);
  shq('git', ['fetch', 'origin', baseBranch]);
  const refOk = shq('git', ['rev-parse', '--verify', ref]).ok;
  const headish = refOk ? ref : head;
  const mb = shq('git', ['merge-base', headish, `origin/${baseBranch}`]);
  const mergeBase = mb.ok ? mb.out.trim() : null;

  const facts = { pr: Number(pr), head, mergeBase, diffstat: null, numstat: {}, files: {}, blobs: {}, shaInfo: {}, runs: {}, identifierHits: {} };

  if (mergeBase) {
    const ns = shq('git', ['diff', '--numstat', `${mergeBase}..${headish}`]);
    if (ns.ok) {
      let ins = 0; let del = 0; let files = 0;
      for (const line of ns.out.split('\n')) {
        const m = line.match(/^(\d+|-)\t(\d+|-)\t(.+)$/);
        if (!m) continue;
        files += 1;
        const i = m[1] === '-' ? 0 : Number(m[1]);
        const d = m[2] === '-' ? 0 : Number(m[2]);
        ins += i; del += d;
        facts.numstat[m[3]] = { insertions: i, deletions: d };
      }
      facts.diffstat = { files, insertions: ins, deletions: del };
    }
  }

  const claims = extract(body);

  // Per-file measurements, four instruments each.
  const wanted = new Set();
  for (const b of claims.byteFigures) for (const p of b.paths) wanted.add(p);
  for (const c of claims.lineCites) wanted.add(c.path);
  for (const p of Object.keys(facts.numstat)) wanted.add(p);
  for (const p of wanted) {
    const blob = shq('git', ['rev-parse', `${headish}:${p}`]);
    if (!blob.ok) continue;
    const content = shq('git', ['show', `${headish}:${p}`], { encoding: 'buffer' });
    if (!content.ok) continue;
    const buf = content.out;
    const text = buf.toString('utf8');
    const rows = text.split('\n');
    const lineAt = {};
    for (const c of claims.lineCites) if (c.path === p && rows[c.cited - 1] !== undefined) lineAt[c.cited] = rows[c.cited - 1];
    let diskBytes = null;
    try { diskBytes = fs.statSync(p).size; } catch (e) { diskBytes = null; }
    facts.files[p] = {
      blob: blob.out.trim(), blobBytes: buf.length, blobChars: Array.from(text).length,
      blobLines: text.endsWith('\n') ? rows.length - 1 : rows.length, diskBytes, lineAt,
    };
  }

  for (const sha of new Set(claims.shas.map((s) => s.sha))) {
    const ty = shq('git', ['cat-file', '-t', sha]);
    if (!ty.ok) { facts.shaInfo[sha] = { resolves: false, type: null, onPrRef: false, subject: null }; continue; }
    const type = ty.out.trim();
    if (type === 'blob') {
      facts.shaInfo[sha] = { resolves: true, type, onPrRef: false, subject: null };
      const content = shq('git', ['cat-file', 'blob', sha], { encoding: 'buffer' });
      if (content.ok) {
        const text = content.out.toString('utf8');
        const rows = text.split('\n');
        facts.blobs[sha] = { bytes: content.out.length, chars: Array.from(text).length, lines: text.endsWith('\n') ? rows.length - 1 : rows.length };
      }
      continue;
    }
    if (type !== 'commit') { facts.shaInfo[sha] = { resolves: true, type, onPrRef: false, subject: null }; continue; }
    const subj = shq('git', ['log', '-1', '--format=%s', sha]);
    facts.shaInfo[sha] = {
      resolves: true,
      type,
      onPrRef: refOk ? shq('git', ['merge-base', '--is-ancestor', sha, ref]).ok : false,
      subject: subj.ok ? subj.out.trim() : null,
    };
  }

  const repoName = o.repo || currentRepo();
  for (const id of new Set(claims.runs.map((r) => r.id))) {
    const r = shq('gh', ['run', 'view', id, ...repoArgs, '--json', 'headSha,conclusion,status,createdAt']);
    let j = null;
    if (r.ok) { try { j = JSON.parse(r.out); } catch (e) { j = null; } }
    if (!j) { facts.runs[id] = { exists: false }; continue; }
    const att = repoName ? shq('gh', ['api', `repos/${repoName}/actions/runs/${id}`, '--jq', '.run_attempt']) : { ok: false };
    facts.runs[id] = { exists: true, headSha: j.headSha, conclusion: j.conclusion, status: j.status, createdAt: j.createdAt, runAttempt: att.ok ? att.out.trim() : '?' };
  }

  // Identifiers: ONE `git grep -o` pass per chunk, tallied. `-o` prints the matched text
  // per hit, so a single pass attributes hits to tokens without one process per token.
  const tokens = [...new Set(claims.identifiers.map((i) => i.token))];
  if (tokens.length) {
    for (const t of tokens) facts.identifierHits[t] = 0;
    const roots = GREP_ROOTS.filter((r) => shq('git', ['cat-file', '-e', `${headish}:${r}`]).ok);
    if (roots.length) {
      for (let i = 0; i < tokens.length; i += 60) {
        const args = ['grep', '-o', '-h', '-F'];
        for (const t of tokens.slice(i, i + 60)) { args.push('-e', t); }
        args.push(headish, '--', ...roots);
        const g = shq('git', args);
        for (const line of String(g.out).split('\n')) {
          const t = line.trim();
          if (facts.identifierHits[t] !== undefined) facts.identifierHits[t] += 1;
        }
      }
    }
  }

  return { body, facts, headish, mergeBase };
}

function currentRepo() {
  const r = shq('gh', ['repo', 'view', '--json', 'nameWithOwner', '--jq', '.nameWithOwner']);
  return r.ok ? r.out.trim() : '';
}

// ---------------------------------------------------------------------------
// Report
// ---------------------------------------------------------------------------

function render(result, ctx) {
  const L = [];
  L.push(`pr-body-check — PR #${ctx.pr}, head ${short(ctx.head)}${ctx.mergeBase ? `, merge base ${short(ctx.mergeBase)}` : ''}`);
  L.push(`claims read: ${Object.keys(result.claims).map((k) => `${k}=${result.claims[k]}`).join(' ')}`);
  L.push('');
  if (!result.findings.length) L.push('no findings.');
  for (const f of result.findings) {
    L.push(`${f.severity.padEnd(8)} ${f.check.padEnd(12)} L${String(f.line).padStart(4)}  ${f.message}`);
    if (f.detail) for (const d of String(f.detail).split('\n')) L.push(`${' '.repeat(28)}| ${d.slice(0, 200)}`);
  }
  L.push('');
  L.push(`SUMMARY pr-body-check #${ctx.pr} @ ${short(ctx.head)}: ${result.counts.MISMATCH} MISMATCH, ${result.counts.CHECK} CHECK`);
  return L.join('\n');
}

function parseArgs(argv) {
  const o = { pr: null, repo: null, bodyFile: null, factsFile: null, diffFile: null, baseBranch: null, listClaims: false, json: false, help: false };
  for (let i = 0; i < argv.length; i += 1) {
    const a = argv[i];
    if (a === '--pr') o.pr = Number(argv[++i]);
    else if (a === '--repo') o.repo = argv[++i];
    else if (a === '--body-file') o.bodyFile = argv[++i];
    else if (a === '--facts') o.factsFile = argv[++i];
    else if (a === '--diff-file') o.diffFile = argv[++i];
    else if (a === '--base') o.baseBranch = argv[++i];
    else if (a === '--list-claims') o.listClaims = true;
    else if (a === '--json') o.json = true;
    else if (a === '--help' || a === '-h') o.help = true;
    else throw new Error(`unknown argument: ${a}`);
  }
  return o;
}

const USAGE = `pr-body-check — re-measure a posted PR body's receipts against its head (#2168 S1)

  node scripts/pr-body-check.cjs --pr <n> [--repo owner/name] [--base main] [--json]
  node scripts/pr-body-check.cjs --pr <n> --list-claims
  node scripts/pr-body-check.cjs --body-file <f> --facts <f.json> [--json]   # offline

Exits 0 always. MISMATCH must be zero before report(done); CHECK rows are sentences to
re-read. Every byte figure is reported against four instruments (blob bytes, on-disk
bytes, blob chars, blob lines), because "N bytes" is true of one and false of another.`;

function main(argv) {
  const o = parseArgs(argv);
  if (o.help) { process.stdout.write(`${USAGE}\n`); return 0; }

  if (o.listClaims) {
    let added;
    if (o.diffFile) added = addedProseFromDiff(fs.readFileSync(o.diffFile, 'utf8'));
    else {
      if (!o.pr) throw new Error('--list-claims needs --pr or --diff-file');
      const meta = JSON.parse(sh('gh', ['pr', 'view', String(o.pr), ...(o.repo ? ['--repo', o.repo] : []), '--json', 'baseRefName']));
      const baseBranch = o.baseBranch || meta.baseRefName || 'main';
      const ref = `refs/tmp/prbc${o.pr}`;
      shq('git', ['fetch', 'origin', `+refs/pull/${o.pr}/head:${ref}`]);
      shq('git', ['fetch', 'origin', baseBranch]);
      const mb = shq('git', ['merge-base', ref, `origin/${baseBranch}`]);
      added = addedProseFromDiff(shq('git', ['diff', `${mb.out.trim()}..${ref}`]).out);
    }
    const rows = listClaims(added);
    if (o.json) { process.stdout.write(`${JSON.stringify(rows, null, 2)}\n`); return 0; }
    process.stdout.write(`pr-body-check --list-claims: ${rows.length} claim shapes in the ADDED prose of the diff\n`
      + 'Each is a claim about a list, a count or an absence that the diff itself may have just made false.\n'
      + 'Re-derive every one at head, and grep its distinctive noun for the twin on another surface.\n\n');
    for (const r of rows) process.stdout.write(`${r.kind.padEnd(9)} ${r.file}\n          "${r.match}"  ${r.text}\n`);
    return 0;
  }

  let body; let facts; let ctx;
  if (o.bodyFile || o.factsFile) {
    if (!o.bodyFile || !o.factsFile) throw new Error('--body-file and --facts go together');
    body = fs.readFileSync(o.bodyFile, 'utf8');
    facts = JSON.parse(fs.readFileSync(o.factsFile, 'utf8'));
    ctx = { pr: facts.pr, head: facts.head, mergeBase: facts.mergeBase };
  } else {
    if (!o.pr) { process.stdout.write(`${USAGE}\n`); return 0; }
    const g = gather(o.pr, o);
    body = g.body; facts = g.facts;
    ctx = { pr: o.pr, head: facts.head, mergeBase: facts.mergeBase };
  }

  const result = analyze(body, facts);
  if (o.json) process.stdout.write(`${JSON.stringify(Object.assign({}, ctx, result), null, 2)}\n`);
  else process.stdout.write(`${render(result, ctx)}\n`);
  return 0;
}

module.exports = {
  extract, analyze, listClaims, addedProseFromDiff, groupQuantities, classifySha,
  isIdentifierToken, tagLines, pathsOn, parseArgs, render, gather, main,
  GREP_ROOTS, ALLOWED_HTML_COMMENTS, CLAIM_SHAPES, SHA_ROLE_PHRASES, PLACEHOLDER_WORDS, USAGE,
};

if (require.main === module) {
  // Exit 0 always — this is a report, never a gate. A crash in the checker must not read
  // as a body defect, so it goes to stderr and the exit code stays 0.
  try { process.exitCode = main(process.argv.slice(2)); }
  catch (err) { process.stderr.write(`pr-body-check: ${(err && err.message) || String(err)}\n`); process.exitCode = 0; }
}

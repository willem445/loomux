// Every embed view that can be WOKEN from outside registers a `hide` hook (#1318).
//
// THE DEFECT THIS EXISTS FOR. `EmbedEntry.hide` carried its rule as prose — "stops the
// follow poll on close/eviction, which every POLLING view has to answer for" — and prose
// that names a MECHANISM does not cover a view that reaches the same cost by another one.
// Three views proved it in the same session: the task board and the NEEDS-YOU panel are
// woken by Tauri event streams and registered no `hide` at all, so both refetched and
// rebuilt off screen on every agent write for the life of the session; the audit log DOES
// poll, and was still missed, because its `setInterval` is armed by a follow toggle rather
// than by `show()` and nobody re-read the sentence against a close. The rule restated —
// *if something outside a view can make it do work, `hide` is where it says what happens
// when nobody is looking* — is now enforced here rather than only asserted in a doc.
//
// WHY A MANIFEST AND NOT A PURE SCAN, and what each half is allowed to decide. The
// manifest DECLARES which kinds are woken, with the reason in the row; the scan is the
// thing that stops a declaration going stale. The scan may only ever ADD to the woken set,
// never subtract from it: a view that gains a `listen()` or a `setInterval` while declared
// quiet fails, and a view declared woken by an INDIRECT waker the scan structurally cannot
// see keeps that declaration. `FileEditView` is exactly that case — its `ft-search`
// subscription is made through `fileapi.ts`'s `onSearchBatch` wrapper, so no `listen(`
// appears in its own source — and it is why the scan is not the authority here.
//
// STATED BLIND SPOTS. The scan reads each view's own file only: a waker reached through a
// helper module is invisible to it (hence `indirectWaker`), and so is one registered from a
// dynamic event name. It also reads `pane.ts` textually — an `embedRegistry.set` call
// assembled any way other than a literal object at a literal kind is not something it can
// name, so it refuses to pass over one instead, by requiring the set of parsed entries to
// be exactly `EMBED_KINDS`.
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

const PANE = readFileSync(new URL("../src/pane.ts", import.meta.url), "utf8");

interface ViewRow {
  /** The `EmbedKind` string, exactly as `embedRegistry.set` spells it. */
  kind: string;
  /** Repo-relative source of the view class this kind registers. */
  source: string;
  /** Declared: can anything outside this view make it do work? */
  woken: boolean;
  /** True when the waker is real but reached through a helper module, so the
   *  per-file scan below cannot see it. Requires `woken`. */
  indirectWaker?: boolean;
  why: string;
}

const VIEWS: ViewRow[] = [
  {
    kind: "tasks",
    source: "src/tasksview.ts",
    woken: true,
    why: "orch-tasks-changed + orch-questions-changed, both agent-driven; each wake is up to four backend reads and a rebuild that is super-linear in the board. Gated by src/wakegate.ts (#1318).",
  },
  {
    kind: "decisions",
    source: "src/decisionsview.ts",
    woken: true,
    why: "orch-questions-changed + orch-tasks-changed + orch-needs-you-changed, and one board write can fire two of them (the demo-gate hook inside upsert_task). Gated by src/wakegate.ts (#1318).",
  },
  {
    kind: "audit",
    source: "src/auditview.ts",
    woken: true,
    why: "a 1.5 s live-follow setInterval. Opt-in and cleared on dispose, but neither of those is a close, so a closed-while-following panel polled orch_audit for the rest of the session until #1318 wired hide().",
  },
  {
    kind: "timeline",
    source: "src/timelineview.ts",
    woken: true,
    why: "the same 1.5 s follow toggle as the audit log's, and the view that had this wiring right first (#361 rev-38).",
  },
  {
    kind: "group",
    source: "src/groupview.ts",
    woken: true,
    why: "a 2 s poll started by show() itself — nine invokes a tick. #361 rev-38 NB2 is where this hook came from.",
  },
  {
    kind: "editor",
    source: "src/fileedit.ts",
    woken: true,
    indirectWaker: true,
    why: "the ft-search batch stream, subscribed through fileapi.ts's onSearchBatch wrapper rather than a listen() in this file. hide() already cleared its search timer pre-#361.",
  },
  {
    kind: "git",
    source: "src/gitview.ts",
    woken: false,
    why: "nothing outside it wakes it: git-changed is handled in pane.ts, which calls the view's own refresh through refreshthrottle.ts. Its hide() exists for a different reason — dismissing an open context menu.",
  },
  {
    kind: "issues",
    source: "src/issuesview.ts",
    woken: false,
    why: "a plain list refreshed by a mode switch and a manual reload button. No stream, no timer.",
  },
];

/** The kinds `pane.ts` itself enumerates — the population every check below is measured
 *  against, read from the source rather than restated here so a new kind cannot be added
 *  to one list and forgotten in the other. */
function embedKinds(): string[] {
  const m = /const EMBED_KINDS: readonly EmbedKind\[\] = \[([\s\S]*?)\];/.exec(PANE);
  assert.ok(m, "EMBED_KINDS is not where this scan expects it in src/pane.ts");
  return [...m[1].matchAll(/"([^"]+)"/g)].map((x) => x[1]);
}

/** Each `this.embedRegistry.set("<kind>", { ... })` entry, brace-balanced from the literal
 *  so a nested arrow body cannot end it early. */
function registryEntries(): Map<string, string> {
  const out = new Map<string, string>();
  for (const m of PANE.matchAll(/this\.embedRegistry\.set\("([^"]+)",\s*\{/g)) {
    const kind = m[1];
    let depth = 1;
    let i = m.index + m[0].length;
    for (; i < PANE.length && depth > 0; i++) {
      if (PANE[i] === "{") depth++;
      else if (PANE[i] === "}") depth--;
    }
    assert.equal(depth, 0, `unbalanced embedRegistry.set literal for "${kind}"`);
    assert.equal(out.has(kind), false, `"${kind}" is registered twice`);
    out.set(kind, PANE.slice(m.index, i));
  }
  return out;
}

/** A wake shape in a view's OWN source: a Tauri event subscription or a fixed-cadence
 *  timer. Deliberately the same two call shapes `test/perfpolicy.test.ts` scans for. */
function directWakers(src: string): number {
  const listens = src.match(/\blisten\s*(?:<[\s\S]*?>)?\s*\(/g) ?? [];
  const intervals = src.match(/\bsetInterval\s*\(/g) ?? [];
  return listens.length + intervals.length;
}

const ENTRIES = registryEntries();
const KINDS = embedKinds();

// ---------- population, before any property is measured over it ----------

test("the scan sees every embed kind pane.ts declares, and nothing else", () => {
  // A guard's green is evidence about its POPULATION first. If `embedRegistry.set` were
  // ever assembled some other way, this scan would quietly watch a smaller set than it
  // claims to — so the parsed set has to BE the declared set, not a subset of it.
  assert.ok(KINDS.length > 0, "EMBED_KINDS parsed as empty — the scan is blind");
  assert.deepEqual([...ENTRIES.keys()].sort(), [...KINDS].sort());
});

test("the manifest covers each kind exactly once", () => {
  const kinds = VIEWS.map((v) => v.kind);
  assert.equal(new Set(kinds).size, kinds.length, "a kind is declared twice");
  assert.deepEqual([...kinds].sort(), [...KINDS].sort());
});

// ---------- the rule ----------

test("every view something can WAKE registers a hide hook", () => {
  const missing = VIEWS.filter((v) => v.woken).filter((v) => !/^\s*hide:/m.test(ENTRIES.get(v.kind)!));
  assert.deepEqual(
    missing.map((v) => v.kind),
    [],
    "a woken view with no `hide` keeps working off screen — see doc/design/embedded-panels.md, 'What `hide` is actually for'"
  );
});

test("verified == matched: every declared-woken kind was actually checked against a parsed entry", () => {
  // The population control the #1327 lesson asks for, at the VERIFIED site: a kind whose
  // entry the scan failed to parse must fail loudly rather than be skipped into a pass.
  const woken = VIEWS.filter((v) => v.woken);
  assert.ok(woken.length > 0, "nothing is declared woken — the rule would be vacuous");
  for (const v of woken) assert.ok(ENTRIES.has(v.kind), `no parsed registry entry for "${v.kind}"`);
});

// ---------- the declaration cannot go stale ----------

test("the scan may only ADD to the woken set: a view that grows a waker must be declared", () => {
  const undeclared = VIEWS.filter((v) => !v.woken).filter(
    (v) => directWakers(readFileSync(new URL(`../${v.source}`, import.meta.url), "utf8")) > 0
  );
  assert.deepEqual(
    undeclared.map((v) => v.kind),
    [],
    "this view gained a listen()/setInterval since it was declared quiet — declare it woken and give it a hide hook"
  );
});

test("a woken row the scan cannot corroborate says so — and the scan is not blind", () => {
  // The vacuity control this file most needs: if `directWakers` matched nothing anywhere,
  // the test above would pass over a whole population of real wakers. So count what it
  // actually found, and require every declared-woken row to be either corroborated by the
  // scan or explicitly marked as reached through a helper module.
  let corroborated = 0;
  for (const v of VIEWS.filter((x) => x.woken)) {
    const found = directWakers(readFileSync(new URL(`../${v.source}`, import.meta.url), "utf8"));
    if (found > 0) {
      corroborated++;
      assert.notEqual(v.indirectWaker, true, `"${v.kind}" is marked indirect but its own source has a waker`);
    } else {
      assert.equal(v.indirectWaker, true, `"${v.kind}" is declared woken but nothing in ${v.source} wakes it`);
    }
  }
  assert.ok(corroborated > 0, "directWakers matched nothing in any view — the instrument is broken, not the code");
});

test("every manifest row names a real, non-empty source and argues itself", () => {
  for (const v of VIEWS) {
    const src = readFileSync(new URL(`../${v.source}`, import.meta.url), "utf8");
    assert.ok(src.length > 0, `${v.source} is empty`);
    assert.ok(v.why.trim().length > 20, `"${v.kind}" needs a real reason, not a label`);
    if (v.indirectWaker) assert.equal(v.woken, true, `"${v.kind}": indirectWaker without woken means nothing`);
  }
});

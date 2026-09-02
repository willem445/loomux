// `docs/orchestration.md`'s bounded claims about the `driver:` block, pinned to
// `src/workflow-schema.json` (#1872).
//
// The class this exists for: a `docs/` page is prose, nothing links it to the code it
// describes, and the drift is silent in BOTH directions. #1870's two blocking findings
// were both false sentences on this page that a fully green `npm test` +
// `cargo test --workspace` had no way to see — one of them said the driver's three
// timeouts are *refused* out of range when the engine *clamps* them and refuses the
// three counters instead. This file makes that sentence, and every number beside it,
// fail when either surface moves: the page assigns each policy its family by naming
// that family's FIELDS, and site 3 below checks that membership against the manifest.
// Until #1951 B2 the page named its families by a bare noun instead ("the three
// counters", "the three timeouts"), which the manifest has no counterpart for and
// nothing here could pin — swapping the two nouns reproduced #1870 B1's sentence
// verbatim with this file green.
//
// WHAT IS PINNED. Three sites on the page, all against `sections.driver` in the
// manifest, all default-deny in both directions (an unmatched row or an unmatched
// schema field fails — a rename on either side cannot step over the join):
//
//   1. The bounds TABLE anchored by the `<!-- pinned-to-schema: ... -->` marker in
//      "The review driver's `driver:` block": per field, min, max, default, and
//      refuse-vs-clamp.
//   2. The YAML example directly above that marker: every value it shows for a
//      *ranged* field is that field's own default, which is what the paragraph under
//      it claims.
//   3. The "Review driver" row of the bounds summary further up the page: its two
//      clauses, and per clause the refuse/clamp polarity, the range tokens, and the
//      set of manifest fields the clause names — which is what makes the policy
//      ASSIGNMENT checkable rather than only the numbers beside it.
//
// WHAT IS NOT PINNED — stated structurally, because the honest residual is a shape and
// not a list of sentences someone remembered to check:
//
//   * Any bound stated on this page OUTSIDE those three sites. The test locates its
//     subjects by marker and by the summary row's own lead-in; a fourth statement of
//     the same numbers, in a paragraph anywhere else on the page, is invisible to it.
//     That is the rule a docs editor has to keep: a new bounded claim about `driver:`
//     goes in the table, not in fresh prose.
//   * Every OTHER row of that same bounds summary. Site 3 below reads the one row whose
//     subject is `driver:`; its siblings state bounds for other blocks and are pinned by
//     nothing. Concretely: editing the "Lock resources" row's `slots` from 1–64 to
//     1–128 reddens no test, here or in `test/workflowschema.test.ts` (the pre-existing
//     manifest-vs-pane pins never read this page). Each is a slice of the same shape.
//   * Every `help:` string in the manifest. The docs prose paraphrases those rather
//     than quoting them, so nothing here compares them.
//   * Any file other than `docs/orchestration.md`. `doc/design/review-driver.md`
//     restates these numbers in two places and NEITHER is pinned: §5.3's example shows
//     the seven defaults, and §2.3 ("Three consequences that are decisions, not
//     defaults") states the counters' ranges as `1..=3` and `0..=1` because its whole
//     subject is why those are refused rather than clamped. §5.3's per-field range
//     comments were removed so that file carries the ranges once rather than twice, but
//     "removed from the example" is not "purged from the file" (#1951 B1). The marker
//     mechanism is page-agnostic and a second page can adopt it; this file declares one
//     page and checks that one.
//   * The round-trip promise about ticking the driver on and off (#1949, the
//     "For one flip on an existing block" paragraph). That is a claim about a
//     BEHAVIOUR, not a bound; it has no counterpart in the manifest and no mechanical
//     link to the tests that do cover the toggle. This file does not reach it.
//   * Whether the MANIFEST agrees with the ENGINE. That is the other half of the
//     contract and it is pinned on the Rust side
//     (`the_workflow_schema_manifest_matches_the_engines_raw_types`). A manifest that
//     had drifted from `parse_workflow` would satisfy every assertion below, because
//     every assertion below reads the manifest as the statement of record.
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

interface SchemaField {
  name: string;
  type: string;
  default?: unknown;
  min?: number;
  max?: number;
  on_out_of_range?: "refuse" | "clamp";
}

/** The directory every path in this file resolves against: the parent of this file's
 *  own directory, which is the repo root while this file sits directly in `test/`. */
const ROOT = new URL("../", import.meta.url);
const manifest: { sections: Record<string, { fields: SchemaField[] }> } = JSON.parse(
  readFileSync(new URL("src/workflow-schema.json", ROOT), "utf8")
);
const DOCS = readFileSync(new URL("docs/orchestration.md", ROOT), "utf8").replace(
  /\r\n/g,
  "\n"
);

const DRIVER: SchemaField[] = manifest.sections.driver.fields;
/** This file's own path relative to `ROOT`, so the marker's pointer is checked against
 *  what the file IS rather than against a literal that a rename moves in lockstep with.
 *  Moving this file deeper would leave `SELF` no longer repo-relative — but the two
 *  `readFileSync(new URL(..., ROOT))` calls above would throw first, loudly (#1951 N3). */
const SELF = decodeURIComponent(import.meta.url.slice(ROOT.href.length));
/** A field is in the RANGE population because it declares one, never because of its
 *  name — a rename cannot move a field in or out of this set. */
const ranged = (f: SchemaField) => f.min !== undefined || f.max !== undefined;
/** The docs spell "not applicable" as an em dash in both range-bearing columns. */
const NA = "—";
/** Any of ASCII hyphen, en dash, em dash may separate a range's two ends. */
const DASH = "[-–—]";

/** `| a | b | c |` -> `["a", "b", "c"]`; anything else -> null. */
function tableRow(line: string): string[] | null {
  const t = line.trim();
  if (!t.startsWith("|") || !t.endsWith("|")) return null;
  return t.slice(1, -1).split("|").map((c) => c.trim());
}
const unbacktick = (s: string) => s.replace(/^`(.*)`$/, "$1");
/** Every `<lo><dash><hi>` token in a piece of prose, normalised to a pair. */
function rangeTokens(s: string): string[] {
  return (s.match(new RegExp(`\\d+\\s*${DASH}\\s*\\d+`, "g")) ?? []).map((t) =>
    t.replace(new RegExp(`\\s*${DASH}\\s*`), "-")
  );
}
const rangeOf = (f: SchemaField) => `${f.min}-${f.max}`;

test("the manifest still carries the driver fields this file pins", () => {
  // Vacuity control for every assertion below: they all iterate `DRIVER`, and an
  // empty or truncated section would make each of them pass over nothing.
  assert.ok(DRIVER.length >= 7, `driver fields: ${DRIVER.length}`);
  assert.ok(
    DRIVER.filter(ranged).length >= 6,
    `ranged driver fields: ${DRIVER.filter(ranged).length}`
  );
  for (const f of DRIVER) {
    if (ranged(f)) {
      assert.equal(typeof f.min, "number", `${f.name}.min`);
      assert.equal(typeof f.max, "number", `${f.name}.max`);
      assert.ok(
        f.on_out_of_range === "refuse" || f.on_out_of_range === "clamp",
        `${f.name}.on_out_of_range: ${String(f.on_out_of_range)}`
      );
    }
    assert.notEqual(f.default, undefined, `${f.name}.default`);
  }
});

test("the docs bounds table states exactly the manifest's driver bounds", () => {
  const marker = /<!-- pinned-to-schema: sections\.driver - (\S+) \(#(\d+)\) -->/g;
  const hits = [...DOCS.matchAll(marker)];
  assert.equal(hits.length, 1, "exactly one pinned-to-schema marker for sections.driver");
  // The marker names this file. Renaming the test without moving the pointer leaves a
  // docs page claiming a pin held by a file that no longer exists, so the pointer is
  // pinned too — against this file's OWN path, derived from `import.meta.url`. A
  // hardcoded literal here would not do it: rename the file and both sides still spell
  // the old name, so the pin would read green while the docs cite nothing (#1951 F1).
  assert.equal(hits[0][1], SELF);

  const after = DOCS.slice(hits[0].index! + hits[0][0].length);
  const rows: string[][] = [];
  let seen = false;
  for (const line of after.split("\n")) {
    const cells = tableRow(line);
    if (cells === null) {
      if (seen) break;
      assert.equal(line.trim(), "", "only blank lines may sit between marker and table");
      continue;
    }
    seen = true;
    rows.push(cells);
  }
  assert.ok(rows.length >= 3, `table rows found after the marker: ${rows.length}`);
  assert.deepEqual(rows[0], ["Field", "Range", "Default", "Outside the range"]);
  assert.ok(
    rows[1].every((c) => /^-+$/.test(c)),
    `second row is the delimiter: ${JSON.stringify(rows[1])}`
  );
  const body = rows.slice(2);

  const byName = new Map(DRIVER.map((f) => [f.name, f]));
  const rowNames = body.map((r) => unbacktick(r[0]));
  assert.equal(new Set(rowNames).size, rowNames.length, "no duplicate rows");
  // DEFAULT-DENY, both directions: every manifest field must have a row and every row
  // must name a manifest field. Either side renaming a field fails here.
  assert.deepEqual(
    [...rowNames].sort(),
    DRIVER.map((f) => f.name).sort(),
    "table rows and manifest driver fields are the same set"
  );

  let matched = 0;
  let verified = 0;
  let rangedVerified = 0;
  const polarity: Record<string, number> = { refuse: 0, clamp: 0 };
  for (const row of body) {
    assert.equal(row.length, 4, `row has four cells: ${JSON.stringify(row)}`);
    const field = byName.get(unbacktick(row[0]));
    assert.ok(field, `row names a manifest field: ${row[0]}`);
    matched++;
    const [, rangeCell, defaultCell, oorCell] = row;
    if (ranged(field)) {
      assert.match(
        rangeCell,
        new RegExp(`^\\d+${DASH}\\d+$`),
        `${field.name} range cell shape`
      );
      assert.equal(rangeTokens(rangeCell)[0], rangeOf(field), `${field.name} range`);
      assert.equal(oorCell, field.on_out_of_range, `${field.name} out-of-range policy`);
      polarity[field.on_out_of_range as string]++;
      rangedVerified++;
    } else {
      assert.equal(rangeCell, NA, `${field.name} has no range, so the cell is n/a`);
      assert.equal(oorCell, NA, `${field.name} has no range, so no policy applies`);
    }
    assert.equal(defaultCell, String(field.default), `${field.name} default`);
    // Counted at the VERIFIED site, after every assertion for this row has run — a row
    // the loop merely matched and then skipped would not reach here (#1327).
    verified++;
  }
  assert.equal(matched, verified, "every matched row was fully verified");
  assert.ok(verified >= 6, `rows verified: ${verified}`);
  assert.ok(rangedVerified >= 6, `ranged rows verified: ${rangedVerified}`);
  assert.deepEqual(
    polarity,
    {
      refuse: DRIVER.filter((f) => f.on_out_of_range === "refuse").length,
      clamp: DRIVER.filter((f) => f.on_out_of_range === "clamp").length,
    },
    "the table's refuse/clamp split is the manifest's"
  );
});

test("the docs YAML example shows every ranged driver field at its default", () => {
  const marker = DOCS.indexOf("<!-- pinned-to-schema: sections.driver");
  assert.ok(marker > 0, "marker present");
  const before = DOCS.slice(0, marker);
  const fenceEnd = before.lastIndexOf("\n```");
  const fenceStart = before.lastIndexOf("\n```yaml\n");
  assert.ok(fenceStart > 0 && fenceEnd > fenceStart, "a yaml fence precedes the marker");
  const yaml = before.slice(fenceStart + "\n```yaml\n".length, fenceEnd);

  const byName = new Map(DRIVER.map((f) => [f.name, f]));
  const shown = new Map<string, string>();
  for (const line of yaml.split("\n")) {
    const m = /^ {2}([a-z_]+):\s*(\S+)\s*$/.exec(line);
    if (m === null) {
      assert.match(line, /^driver:$|^$/, `unparsed line in the example: ${line}`);
      continue;
    }
    shown.set(m[1], m[2]);
  }
  // DEFAULT-DENY, both directions again: the example is the whole block.
  assert.deepEqual(
    [...shown.keys()].sort(),
    DRIVER.map((f) => f.name).sort(),
    "the example names exactly the manifest's driver fields"
  );

  let matched = 0;
  let verified = 0;
  let exempt = 0;
  for (const [name, raw] of shown) {
    const field = byName.get(name);
    assert.ok(field, `example line names a manifest field: ${name}`);
    matched++;
    if (ranged(field)) {
      assert.equal(Number(raw), field.default, `${name} shown at its default`);
    } else {
      // The example deliberately shows the block turned ON. A field is exempt from the
      // value pin because it has no range (structural), and the exemption has to be
      // load-bearing: a field shown AT its default would need no exemption at all.
      assert.notEqual(raw, String(field.default), `${name} is shown off its default`);
      exempt++;
    }
    verified++;
  }
  assert.equal(matched, verified, "every field in the example was verified");
  assert.ok(verified >= 7, `example fields verified: ${verified}`);
  assert.equal(
    exempt,
    DRIVER.length - DRIVER.filter(ranged).length,
    "only the un-ranged fields are exempt from the default pin"
  );
});

test("the bounds summary's driver row assigns each policy the manifest's own fields", () => {
  // The row that #1870 B1 got backwards: it assigned `refused` to the timeout family.
  // It used to identify its two families by a bare noun ("the three counters", "the
  // three timeouts"), and a noun is not something the manifest has, so nothing could
  // pin it: swapping the two nouns and leaving every range and policy word attached to
  // the clause it was already in reproduced #1870 B1's exact sentence with this file
  // green (#1951 B2). The row now names its FIELDS, as every sibling row on that page
  // already does, so each family is identified by its members and the assignment is
  // checkable end to end.
  const lead = "\n- **Review driver** (`driver:`):";
  const at = DOCS.indexOf(lead);
  assert.ok(at > 0, "the bounds summary still has a Review driver row");
  const rest = DOCS.slice(at + 1);
  const end = rest.search(/\n(?:- |\n)/);
  const rowText = (end === -1 ? rest : rest.slice(0, end)).replace(/\s+/g, " ");

  // Slice past the row's own lead-in. Splitting on the first ":" would land inside the
  // `driver:` code span, which is itself one of the row's backticked tokens.
  const leadEnd = rowText.indexOf("`): ");
  assert.ok(leadEnd > 0, `row keeps its \`driver:\` lead-in: ${rowText}`);
  const clauses = rowText.slice(leadEnd + "`): ".length).split(";");
  assert.equal(clauses.length, 2, `two clauses: ${JSON.stringify(rowText)}`);

  const stems = ["refuse", "clamp"];
  assert.deepEqual(
    [...new Set(DRIVER.filter(ranged).map((f) => f.on_out_of_range))].sort(),
    [...stems].sort(),
    "the manifest's driver policies are exactly the two this row can express"
  );

  let matched = 0;
  let verified = 0;
  const covered: string[] = [];
  for (const clause of clauses) {
    // The clause's POLARITY is read off the clause, never off its position — reordering
    // the two clauses is a rewrite, not a regression.
    const found = stems.filter((s) => clause.includes(s));
    assert.equal(found.length, 1, `exactly one policy word in: ${clause}`);
    matched++;
    const policy = found[0];
    const family = DRIVER.filter((f) => f.on_out_of_range === policy);
    assert.deepEqual(
      [...new Set(rangeTokens(clause))].sort(),
      [...new Set(family.map(rangeOf))].sort(),
      `${policy} clause states exactly that family's ranges`
    );
    // DEFAULT-DENY on the membership itself: every backticked token in the clause must
    // be a manifest driver field, and the set must be exactly this policy's family. A
    // field moved to the wrong clause, misspelled, or dropped fails here — which is the
    // #1870 B1 defect shape, now expressed in the vocabulary the manifest actually has.
    const named = [...clause.matchAll(/`([A-Za-z_]+)`/g)].map((m) => m[1]);
    assert.equal(new Set(named).size, named.length, `no field named twice: ${clause}`);
    assert.deepEqual(
      [...named].sort(),
      family.map((f) => f.name).sort(),
      `${policy} clause names exactly that family's fields`
    );
    covered.push(policy);
    verified++;
  }
  assert.equal(matched, verified, "every clause was fully verified");
  assert.deepEqual([...covered].sort(), [...stems].sort(), "both policies are covered");
});

// The frontend half of `src/workflow-schema.json`'s contract (#880).
//
// The manifest is the one committed statement of what a `.loomux/workflow.yml` field
// IS. The engine's half of enforcing it lives in `src-tauri/tests/orchestration.rs`
// (`the_workflow_schema_manifest_matches_the_engines_raw_types`): manifest sections,
// field for field, against the `Raw*` serde types. This half asks the other question,
// the one that produced the bug the manifest exists for — `allow:` was a real block
// field for a year and the pane had no name for it, so a workflow that declared one
// looked exactly like a workflow that didn't:
//
//   (a) the PARSER knows every field — it never lands in the unknown-key bag;
//   (b) the canonical SERIALIZER emits every field — a save can't silently drop one;
//   (c) every field is either claimed by a form control or explicitly listed as not
//       yet having one (the list slice A owns and slice C empties).
//
// All three are behavioral: the assertions drive real text through the real parser and
// serializer rather than reading the model's own key sets back to it, so a test that
// passes is a field a human can actually round-trip through the pane.
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import {
  analyzeWorkflow,
  parseWorkflow,
  roleHintRequires,
  serializeWorkflow,
  RESOURCE_SLOTS_MIN,
  RESOURCE_SLOTS_MAX,
  RESOURCE_MAX_HOLD_MINUTES_MIN,
  RESOURCE_MAX_HOLD_MINUTES_MAX,
  RESOURCES_MAX,
  POLICY_BOUNDS,
  type Workflow,
} from "../src/workflowmodel.ts";
import { DEFAULT_HOLD } from "../src/issuesmodel.ts";
import { fullAutonomyHelp } from "../src/autonomy.ts";

interface SchemaField {
  name: string;
  type: string;
  values?: string[];
  section?: string;
  help?: string;
  default?: unknown;
  required?: boolean;
  min?: number;
  max?: number;
  /** How many entries a `section-map` may hold (`workflow.resources`). */
  max_entries?: number;
  /** What the ENGINE does with a value outside `min`/`max` — refuse the whole file, or
   *  quietly pull it into range. Pinned behaviorally against `parse_workflow` on the Rust
   *  side; the pane's half is that the two produce different severities (#1020). */
  on_out_of_range?: "refuse" | "clamp";
}
interface SchemaSection {
  title: string;
  help: string;
  fields: SchemaField[];
}
interface Manifest {
  types: Record<string, string>;
  sections: Record<string, SchemaSection>;
}

const manifest: Manifest = JSON.parse(
  readFileSync(new URL("../src/workflow-schema.json", import.meta.url), "utf8")
);

/** The container types: a field whose value IS another section. They are the shape of
 *  the file rather than a value a human types, so the round-trip and editor questions
 *  below are asked of what is INSIDE them, not of them. */
const STRUCTURAL = new Set(["section", "section-list", "section-map"]);

const sections = Object.entries(manifest.sections);
const leafFields = (): { section: string; field: SchemaField; id: string }[] =>
  sections.flatMap(([section, s]) =>
    s.fields
      .filter((f) => !STRUCTURAL.has(f.type))
      .map((f) => ({ section, field: f, id: `${section}.${f.name}` }))
  );

// ---------- the manifest is well-formed ----------

test("every manifest field declares a type this build knows and help a human can read", () => {
  assert.ok(sections.length > 0, "a manifest with no sections describes nothing");
  for (const [name, section] of sections) {
    assert.ok(section.title, `${name}: needs a title`);
    assert.ok(section.help, `${name}: needs help text — the manifest is what the GUI explains from`);
    assert.ok(section.fields.length > 0, `${name}: needs fields`);
    for (const f of section.fields) {
      const where = `${name}.${f.name}`;
      assert.ok(manifest.types[f.type], `${where}: unknown type "${f.type}"`);
      assert.ok(f.help, `${where}: needs help text`);
      if (f.type === "enum") {
        assert.ok(f.values?.length, `${where}: an enum with no values closes nothing`);
        // `!f.default` would exempt exactly the case that was wrong (#880 review
        // finding 1): `block.cli`'s default is the EMPTY STRING, which is falsy, so
        // the one row whose default sat outside its own values slipped through the
        // check meant to catch it. An absent default is `undefined`, and nothing else.
        assert.ok(
          f.default === undefined || f.values.includes(String(f.default)),
          `${where}: the default ${JSON.stringify(f.default)} is not one of this enum's own values — a select generated from those values could never show it`
        );
      } else {
        assert.equal(f.values, undefined, `${where}: only an enum carries values`);
      }
      if (STRUCTURAL.has(f.type)) {
        assert.ok(
          f.section && manifest.sections[f.section],
          `${where}: type ${f.type} must name a section that exists`
        );
      } else {
        assert.equal(f.section, undefined, `${where}: only a container names a section`);
      }
    }
  }
});

test("no section is orphaned — every one is reachable from the workflow root", () => {
  // A section nothing points at is a section no form will ever render, and the Rust
  // parity test would keep it green forever: it compares the sections that exist, not
  // the ones the schema can actually reach.
  const reachable = new Set(["workflow"]);
  for (let pass = 0; pass < sections.length; pass++) {
    for (const [name, section] of sections) {
      if (!reachable.has(name)) continue;
      for (const f of section.fields) if (f.section) reachable.add(f.section);
    }
  }
  assert.deepEqual(
    sections.map(([n]) => n).filter((n) => !reachable.has(n)),
    []
  );
});

// ---------- (a) + (b): the pane's model round-trips every field ----------

/** A value for one field, in the YAML the file would carry and the shape the model
 *  should hand back. Deliberately NOT the field's own default: a value equal to the
 *  default can't tell "read it" apart from "filled it in". */
function sampleFor(id: string, f: SchemaField): { yaml: string; model: unknown } {
  const override: Record<string, { yaml: string; model: unknown }> = {
    // The one field whose value this build genuinely constrains: an unsupported
    // version is a finding, and a doc carrying one would test the finding, not the field.
    "workflow.version": { yaml: "1", model: 1 },
    // The far end of the sample edge, so `from`/`to` are distinguishable.
    "edge.to": { yaml: "b2", model: "b2" },
  };
  if (override[id]) return override[id]!;
  switch (f.type) {
    case "number":
      return { yaml: "7", model: 7 };
    case "bool":
      return { yaml: "true", model: true };
    case "enum":
      return { yaml: f.values![0]!, model: f.values![0]! };
    case "string-list":
      return { yaml: '["Bash(gh pr view --json title,body)", beta]', model: ["Bash(gh pr view --json title,body)", "beta"] };
    case "block-ref-list":
      return { yaml: "[b]", model: ["b"] };
    case "block-ref":
      return { yaml: "b", model: "b" };
    default:
      return { yaml: "sample-value", model: "sample-value" };
  }
}

/** A whole workflow file carrying exactly one field under test, wherever that field
 *  lives. Two blocks, so an edge has two ends. */
function docFor(section: string, field: string, yaml: string): string {
  const roster = ["blocks:", "  - id: b", "    kind: worker", "  - id: b2", "    kind: worker"];
  switch (section) {
    case "workflow":
      return ["version: 1", `${field}: ${yaml}`, ...roster].join("\n") + "\n";
    case "block": {
      // The field under test goes on the FIRST block (the one `readBack` reads), and
      // overrides the base key when it happens to be one of them.
      const base: Record<string, string> = { id: "b", kind: "worker" };
      base[field] = yaml;
      const first = [
        `  - id: ${base.id}`,
        ...Object.keys(base)
          .filter((k) => k !== "id")
          .map((k) => `    ${k}: ${base[k]}`),
      ];
      return (
        ["version: 1", "blocks:", ...first, "  - id: b2", "    kind: worker"].join("\n") + "\n"
      );
    }
    case "edge": {
      const from = field === "from" ? yaml : "b";
      const to = field === "to" ? yaml : "b2";
      return (
        ["version: 1", ...roster, "edges:", `  - { from: ${from}, to: ${to} }`].join("\n") + "\n"
      );
    }
    case "gate":
      return (
        ["version: 1", ...roster, "gates:", "  merge:", `    ${field}: ${yaml}`].join("\n") + "\n"
      );
    // #1176: the field under test is a key on the FIRST routing rule. The rule's
    // other key is left off deliberately — the emitter has to put it back, which
    // is what makes this round-trip mean something for a section-list.
    case "gate.routing":
      return (
        [
          "version: 1",
          ...roster,
          "gates:",
          "  merge:",
          "    routing:",
          `      - ${field}: ${yaml}`,
        ].join("\n") + "\n"
      );
    case "intake":
      return ["version: 1", ...roster, "intake:", `  ${field}: ${yaml}`].join("\n") + "\n";
    case "intake.labels":
      return (
        ["version: 1", ...roster, "intake:", "  labels:", `    ${field}: ${yaml}`].join("\n") + "\n"
      );
    case "merge_queue":
      return ["version: 1", ...roster, "merge_queue:", `  ${field}: ${yaml}`].join("\n") + "\n";
    case "resource":
      return (
        ["version: 1", ...roster, "resources:", "  ci:", `    ${field}: ${yaml}`].join("\n") + "\n"
      );
    case "board":
      return ["version: 1", ...roster, "board:", `  ${field}: ${yaml}`].join("\n") + "\n";
    case "board.wip":
      return (
        ["version: 1", ...roster, "board:", "  wip:", `    ${field}: ${yaml}`].join("\n") + "\n"
      );
    default:
      throw new Error(`this test has no fixture shape for section "${section}"`);
  }
}

/** Where the field's value and its section's unknown-key bag live in the model. */
function readBack(w: Workflow, section: string, field: string): { value: unknown; extra: unknown } {
  const at = (obj: Record<string, unknown> | undefined): { value: unknown; extra: unknown } => ({
    value: obj?.[field],
    // `edge`/`gate` have no bag: an unknown key there is simply not read, so "did the
    // parser know it?" is answered by the value alone.
    extra: (obj as { extra?: Record<string, unknown> } | undefined)?.extra?.[field],
  });
  switch (section) {
    case "workflow":
      return at(w as unknown as Record<string, unknown>);
    case "block":
      return at(w.blocks[0] as unknown as Record<string, unknown>);
    case "edge":
      return at(w.edges[0] as unknown as Record<string, unknown>);
    case "gate":
      return at(w.gates.merge as unknown as Record<string, unknown>);
    case "gate.routing":
      return at(w.gates.merge?.routing?.[0] as unknown as Record<string, unknown>);
    case "intake":
      return at(w.intake as unknown as Record<string, unknown>);
    case "intake.labels":
      return at(w.intake?.labels as unknown as Record<string, unknown>);
    case "merge_queue":
      return at(w.merge_queue as unknown as Record<string, unknown>);
    case "resource":
      return at(w.resources?.ci as unknown as Record<string, unknown>);
    case "board":
      return at(w.board as unknown as Record<string, unknown>);
    // `board.wip` is the one section whose unknown-key bag does not sit on the section
    // itself: the caps are a plain `Record<string, number>`, so an `extra` key inside it
    // would be indistinguishable from a cap. It lives on the parent as `wipExtra`.
    case "board.wip":
      return {
        value: w.board?.wip?.[field],
        extra: (w.board?.wipExtra as Record<string, unknown> | undefined)?.[field],
      };
    default:
      throw new Error(`this test cannot read section "${section}"`);
  }
}

test("the pane's parser knows every field in the manifest — none of them is an unknown key", () => {
  for (const { section, field, id } of leafFields()) {
    const sample = sampleFor(id, field);
    const { workflow } = parseWorkflow(docFor(section, field.name, sample.yaml));
    const read = readBack(workflow, section, field.name);
    assert.deepEqual(
      read.value,
      sample.model,
      `${id}: the parser did not read this field — the pane cannot show, edit or save a key it has no name for`
    );
    assert.equal(read.extra, undefined, `${id}: read as an UNKNOWN key rather than a schema field`);
  }
});

test("the canonical serializer emits every field in the manifest — a save drops nothing", () => {
  for (const { section, field, id } of leafFields()) {
    const sample = sampleFor(id, field);
    const parsed = parseWorkflow(docFor(section, field.name, sample.yaml)).workflow;
    const text = serializeWorkflow(parsed);
    const reread = parseWorkflow(text).workflow;
    assert.deepEqual(
      readBack(reread, section, field.name).value,
      sample.model,
      `${id}: did not survive serialize → parse. The Format action and every form edit go \
through this emitter, so a field it skips is a line the pane deletes:\n${text}`
    );
  }
});

// ---------- (c) which fields have an editor ----------
//
// Slice A owns this list; slice C (descriptor-driven config forms) empties it as each
// control lands, replacing FIELDS_WITH_AN_EDITOR with the real descriptor registry it
// builds.
//
// **These two lists track the DESCRIPTOR REGISTRY, not hand-built forms** — worth saying
// out loud, because the gap is now wide enough to mislead (#1020 review, finding 8). Most
// of `FIELDS_WITHOUT_AN_EDITOR` has had a hand-written control in `workflowview.ts` for a
// long time: `block.id`/`name`/`kind`/`cli`/`model` and `gate.require`/`reviewers`/`also`
// since #222, and `intake.*`, `merge_queue.*`, `resource.*`, `block.allow` and
// `block.role_hint` since #1020. Nothing here compares against an actual control, so an
// entry left behind after its form ships does NOT redden — all four assertions below are
// list hygiene (membership in the manifest, no overlap, no gaps). What still bites, and is
// the reason to keep them: a field added to the schema with no decision recorded about its
// editor is RED.

/** Claimed by a form control today. Slice C fills this from its descriptor registry.
 *
 *  Slice C also has two manifest keys to HONOR, not merely to render (#880 review
 *  finding 4): `on_out_of_range` says whether a number's bound is refused by the engine
 *  (stop the submit) or silently clamped (accept and coerce), and `max_entries` on
 *  `workflow.resources` is a hard cap — an "add a resource" affordance that writes a
 *  33rd entry produces a file the engine refuses whole. Both are pinned against the
 *  engine in `src-tauri/tests/orchestration.rs`, so the data is trustworthy; consuming
 *  it is the renderer's half. */
const FIELDS_WITH_AN_EDITOR = new Set<string>([]);

/** No control yet — every leaf field, as of slice A. */
const FIELDS_WITHOUT_AN_EDITOR = new Set<string>([
  "workflow.version",
  "workflow.name",
  "workflow.authored_with",
  "block.id",
  "block.name",
  "block.kind",
  "block.cli",
  "block.model",
  "block.prompt",
  "block.profile",
  "block.allow",
  "block.role_hint",
  "block.effort",
  "block.context",
  // #1457. The pane READS, EMITS and VALIDATES `remote:` — the three tests above
  // and `workflowmodel.test.ts`'s refusal tests are what check that — but the
  // designer has no control for it yet, and deliberately not: R1 is the schema
  // and its refusals. An affordance for it waits on the operator binding
  // (#1458), which is what turns a label into a list of names worth offering.
  "block.remote",
  "edge.from",
  "edge.to",
  "gate.require",
  "gate.threshold",
  "gate.reviewers",
  "gate.also",
  "gate.max_diff_lines",
  // #1176. The pane READS, EMITS and VALIDATES these — without which a rule
  // would be a line the next form edit silently deleted — and shows them
  // read-only. What it has no control for yet is adding or changing one.
  "gate.routing.paths",
  "gate.routing.reviewers",
  "intake.source",
  "intake.labels.ready",
  "intake.labels.investigate",
  "intake.labels.owned",
  "intake.labels.prototype",
  "intake.labels.hold",
  "merge_queue.enabled",
  "merge_queue.max_batch",
  "merge_queue.checks_timeout_minutes",
  "resource.slots",
  "resource.max_hold_minutes",
  // #1175. The pane PARSES, PRESERVES and RE-EMITS `board:` (that is what the two
  // tests above check) — it just has no generated form for it yet, like every other
  // field on this list.
  "board.enforce",
  "board.wip.queued",
  "board.wip.in-progress",
  "board.wip.review",
  "board.wip.pr",
  "board.wip.prototype",
  "board.wip.human-testing",
  "board.wip.blocked",
]);

test("every schema field is either editable in the pane or listed as not yet editable", () => {
  const ids = leafFields().map((f) => f.id);
  assert.deepEqual(
    [...FIELDS_WITHOUT_AN_EDITOR].filter((id) => !ids.includes(id)),
    [],
    "a listed field that is no longer in the manifest — delete it from the list"
  );
  assert.deepEqual(
    [...FIELDS_WITH_AN_EDITOR].filter((id) => !ids.includes(id)),
    [],
    "a form control for a field the schema doesn't have"
  );
  assert.deepEqual(
    [...FIELDS_WITH_AN_EDITOR].filter((id) => FIELDS_WITHOUT_AN_EDITOR.has(id)),
    [],
    "a field cannot both have an editor and not have one — remove it from the pending list"
  );
  assert.deepEqual(
    ids.filter((id) => !FIELDS_WITH_AN_EDITOR.has(id) && !FIELDS_WITHOUT_AN_EDITOR.has(id)),
    [],
    "a new schema field needs a decision: give it a form control, or list it as pending"
  );
});

// ---------- (c2) the frontend's veto fallback agrees with the engine ----------

test("the pane's hold-label fallback is the engine's built-in veto (#778)", () => {
  // `DEFAULT_HOLD` and `autonomy.ts`'s `holdName` fallback are the only two
  // places the frontend may name the veto by literal: everything user-facing
  // reads the resolved spelling from the backend, and these are what render
  // before the first status read resolves it.
  //
  // A literal that DRIFTS from the engine's built-in would show the wrong veto
  // to every repo that never renamed it — the same defect as the hardcodes this
  // replaced, just with a longer fuse. So it is pinned against the manifest's
  // declared default, which `workflow_schema_field_facts()` pins against
  // `builtin_intake_profile()` on the Rust side: engine → manifest → pane, with
  // no link left to assumption.
  const hold = manifest.sections["intake.labels"]?.fields.find((f) => f.name === "hold");
  assert.ok(hold, "the manifest must declare intake.labels.hold");
  assert.equal(
    DEFAULT_HOLD,
    hold.default,
    "the pane's fallback veto must equal the engine's built-in — a repo that never renamed the \
label would otherwise be shown a veto that holds nothing"
  );
  // The same value must survive the sentence-building fallback in autonomy.ts,
  // which is what actually reaches the panel's help and mode chip.
  assert.ok(
    fullAutonomyHelp("").includes(`you label ${hold.default}`),
    "an unresolved spelling must fall back to the engine's built-in"
  );
});

// ---------- (d) the enum rows are real, in the pane's own opinion ----------
//
// The engine half of this lives in `src-tauri/tests/orchestration.rs`
// (`every_enum_value_the_manifest_declares_is_one_the_engine_accepts`, which feeds each
// declared value through the real `parse_workflow`). This is the pane's half: a value
// the manifest declares must not raise a finding here either, or the GUI would paint a
// legal file red — the same lie as blessing an illegal one, pointed the other way.
//
// Only the fields the pane HAS a rule for are covered, and the gaps are named rather
// than quietly skipped:
//   - `effort` and `context` have no validation rule of their own in this model (they are
//     capability data the backend owns, answered per CLI/model by `agent_cli_knobs`);
//     `intake.source` grew one with #1020, when the pane gained the form that writes it —
//     a picker offering a source the parser refuses is the failure the rule exists to stop;
//   - `block.cli: ""` is legal to the ENGINE (inherit the group's CLI) and the pane
//     deliberately still asks for an explicit one. That asymmetry is left alone here on
//     purpose: a pane stricter than the engine can annoy, but it cannot mislead someone
//     into a file that will not load. Changing it is a product decision, not a manifest
//     fix — carried, and named in the PR.

/** Every finding the pane raises for a workflow whose text is otherwise clean. */
const findingCodesFor = (text: string): string[] =>
  analyzeWorkflow(text).findings.map((f) => f.code);

const manifestValues = (section: string, field: string): string[] => {
  const s = manifest.sections[section];
  assert.ok(s, `${section} must exist in the manifest`);
  const f = s.fields.find((x) => x.name === field);
  assert.ok(f?.values?.length, `${section}.${field} must declare enum values`);
  return f.values;
};

test("every enum value the manifest declares is one the PANE accepts too", () => {
  for (const kind of manifestValues("block", "kind")) {
    const codes = findingCodesFor(`version: 1\nblocks:\n  - id: b\n    kind: ${kind}\n    cli: claude\n`);
    assert.ok(
      !codes.includes("unknown-kind"),
      `block.kind: the manifest declares ${JSON.stringify(kind)} and the pane calls it unknown`
    );
  }
  // The empty value is the engine's, not the pane's — see the note above.
  for (const cli of manifestValues("block", "cli").filter((v) => v !== "")) {
    const codes = findingCodesFor(`version: 1\nblocks:\n  - id: b\n    kind: worker\n    cli: ${cli}\n`);
    assert.ok(
      !codes.includes("unknown-cli"),
      `block.cli: the manifest declares ${JSON.stringify(cli)} and the pane calls it unspawnable`
    );
  }
  for (const hint of manifestValues("block", "role_hint")) {
    const required = roleHintRequires(hint);
    assert.ok(required, `block.role_hint: the manifest declares ${JSON.stringify(hint)}, which the pane does not recognize`);
    const codes = findingCodesFor(
      `version: 1\nblocks:\n  - id: b\n    kind: ${required}\n    cli: claude\n    role_hint: ${hint}\n`
    );
    assert.ok(
      !codes.some((c) => c.startsWith("role-hint")),
      `block.role_hint: ${JSON.stringify(hint)} must pair cleanly with kind ${required}`
    );
  }
  for (const source of manifestValues("intake", "source")) {
    const codes = findingCodesFor(
      `version: 1\nblocks:\n  - id: b\n    kind: worker\n    cli: claude\nintake:\n  source: "${source}"\n`
    );
    assert.ok(
      !codes.includes("intake-unknown-source"),
      `intake.source: the manifest declares ${JSON.stringify(source)} and the pane calls it unknown — including "" , which means the built-in source`
    );
  }
  for (const require of manifestValues("gate", "require")) {
    const threshold = require === "threshold" ? "    threshold: 1\n" : "";
    const codes = findingCodesFor(
      `version: 1\nblocks:\n  - id: rev\n    kind: reviewer\n    cli: claude\ngates:\n  merge:\n    require: ${require}\n${threshold}    reviewers: [rev]\n`
    );
    assert.ok(
      !codes.includes("gate-unknown-require"),
      `gate.require: the manifest declares ${JSON.stringify(require)} and the pane calls it unknown — the engine accepts it (\`all\` is its synonym for \`all-pass\`), so this is the pane painting a legal file red`
    );
  }
});

test("…and a value outside a declared enum is still refused by the pane", () => {
  // The other half of "closed": without this, the test above would pass just as well
  // against a validator that had stopped checking anything at all.
  assert.ok(
    findingCodesFor("version: 1\nblocks:\n  - id: b\n    kind: superuser\n    cli: claude\n").includes("unknown-kind")
  );
  assert.ok(
    findingCodesFor("version: 1\nblocks:\n  - id: b\n    kind: worker\n    cli: notacli\n").includes("unknown-cli")
  );
  assert.equal(roleHintRequires("supervisor"), undefined);
  assert.ok(
    findingCodesFor(
      "version: 1\nblocks:\n  - id: rev\n    kind: reviewer\n    cli: claude\ngates:\n  merge:\n    require: most\n    reviewers: [rev]\n"
    ).includes("gate-unknown-require")
  );
  assert.ok(
    findingCodesFor(
      "version: 1\nblocks:\n  - id: b\n    kind: worker\n    cli: claude\nintake:\n  source: jira\n"
    ).includes("intake-unknown-source")
  );
});

// ---------- (e) the pane's numeric bounds ARE the manifest's (#1020) ----------
//
// The forms for `merge_queue:` and `resources:` clamp what they write, and the validation
// pass flags a hand-written value outside the same range. Both read constants in
// `workflowmodel.ts` — a hand-written mirror, like `WORKFLOW_CLIS` and `GATE_REQUIRES`
// before them, and hand-written for the same reason (that module is pure and import-free,
// so it cannot read the manifest at runtime).
//
// This is the link that makes the mirror safe rather than merely convenient: the Rust side
// pins the manifest's bounds against the engine's own constants
// (`workflow_schema_field_facts`), and this pins the pane's constants against the manifest.
// Engine → manifest → pane, with no step left to assumption. A `RESOURCE_SLOTS_MAX` bumped
// to 128 in the engine reddens the Rust test; one bumped only in the pane reddens this.

const bound = (section: string, field: string): SchemaField => {
  const f = manifest.sections[section]?.fields.find((x) => x.name === field);
  assert.ok(f, `${section}.${field} must exist in the manifest`);
  return f;
};

test("every bound the pane clamps to is the bound the manifest declares (#1020)", () => {
  // Over the TABLE the forms actually read, not over a hand-written list of the fields I
  // remembered to check — that distinction is the whole finding this test grew from
  // (review finding 2): `merge_queue.max_batch` was clamped to a literal `64` in the form,
  // a ceiling no engine constant and no manifest row declares, and the earlier version of
  // this test could not see it because it asserted only the bounds it named.
  for (const [id, bounds] of Object.entries(POLICY_BOUNDS)) {
    // Split on the LAST dot, not the first: a section name may itself contain one
    // (`intake.labels`, `board.wip`), so `id.split(".")` reads `board.wip.queued` as
    // section `board`, field `wip` — a field that does not exist, which the `bound`
    // assertion would then report as a missing manifest row rather than as this
    // helper's own bug.
    const dot = id.lastIndexOf(".");
    const [section, field] = [id.slice(0, dot), id.slice(dot + 1)];
    const declared = bound(section, field);
    assert.equal(bounds.min, declared.min, `${id}: min`);
    // **`max` is compared including its ABSENCE.** A manifest row with no `max` means the
    // engine imposes no ceiling, so a `max` in the table is a number the pane invented —
    // and a form that clamps to an invented ceiling silently rewrites a legal file.
    assert.equal(
      bounds.max,
      declared.max,
      `${id}: max — the manifest ${declared.max === undefined ? "declares NO ceiling, so the pane must not clamp to one" : `declares ${declared.max}`}`
    );
  }
  // The other direction: a manifest bound the forms don't read is a field whose form can
  // write a value the engine refuses. Every numeric field carrying a bound must be in the
  // table (`workflow.version` and the string fields carry none, so they are not expected).
  const boundedFields = sections.flatMap(([section, s]) =>
    s.fields
      .filter((f) => f.type === "number" && (f.min !== undefined || f.max !== undefined))
      .map((f) => `${section}.${f.name}`)
  );
  assert.deepEqual(
    boundedFields.filter((id) => !(id in POLICY_BOUNDS)),
    [],
    "a manifest field with a bound that no form reads — its form can write what the engine refuses"
  );
  // The one cardinality cap: an "add a resource" button that writes a 33rd entry produces a
  // file the engine refuses whole, so the form disables itself at exactly this number.
  assert.equal(bound("workflow", "resources").max_entries, RESOURCES_MAX);
});

test("refuse-vs-clamp is the difference between an error and a warning in the pane (#1020)", () => {
  // The manifest states which of the two the engine does; the pane must say the matching
  // thing, because "your file will not load" and "your file will not do what it says" send a
  // human to different places. A pane that called a clamped value an error would be crying
  // wolf; one that called a refused value a warning would be blessing a file that never loads.
  const cases: { section: string; field: string; text: string }[] = [
    {
      section: "merge_queue",
      field: "max_batch",
      text: "version: 1\nblocks:\n  - id: b\n    kind: worker\n    cli: claude\nmerge_queue:\n  max_batch: 0\n",
    },
    {
      section: "merge_queue",
      field: "checks_timeout_minutes",
      text: "version: 1\nblocks:\n  - id: b\n    kind: worker\n    cli: claude\nmerge_queue:\n  checks_timeout_minutes: 9999\n",
    },
    {
      section: "resource",
      field: "slots",
      text: "version: 1\nblocks:\n  - id: b\n    kind: worker\n    cli: claude\nresources:\n  ci:\n    slots: 9999\n",
    },
    {
      section: "resource",
      field: "max_hold_minutes",
      text: "version: 1\nblocks:\n  - id: b\n    kind: worker\n    cli: claude\nresources:\n  ci:\n    max_hold_minutes: 9999\n",
    },
  ];
  for (const c of cases) {
    const declared = bound(c.section, c.field).on_out_of_range;
    assert.ok(declared, `${c.section}.${c.field}: the manifest must say refuse or clamp`);
    const found = analyzeWorkflow(c.text).findings.filter((f) => f.code === "section-out-of-range");
    assert.equal(found.length, 1, `${c.section}.${c.field}: out of range and unreported`);
    assert.equal(
      found[0]!.severity,
      declared === "refuse" ? "error" : "warning",
      `${c.section}.${c.field}: the manifest says the engine will ${declared} it`
    );
  }
});

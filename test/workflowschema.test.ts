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
  type Workflow,
} from "../src/workflowmodel.ts";

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
    case "intake":
      return at(w.intake as unknown as Record<string, unknown>);
    case "intake.labels":
      return at(w.intake?.labels as unknown as Record<string, unknown>);
    case "merge_queue":
      return at(w.merge_queue as unknown as Record<string, unknown>);
    case "resource":
      return at(w.resources?.ci as unknown as Record<string, unknown>);
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
// builds. Until then the assertions still bite in the direction that matters: a field
// added to the schema with no decision recorded about its editor is RED, and an entry
// left behind after its control ships is RED too.

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
  "edge.from",
  "edge.to",
  "gate.require",
  "gate.threshold",
  "gate.reviewers",
  "gate.also",
  "intake.source",
  "intake.labels.ready",
  "intake.labels.investigate",
  "intake.labels.owned",
  "intake.labels.prototype",
  "merge_queue.enabled",
  "merge_queue.max_batch",
  "merge_queue.checks_timeout_minutes",
  "resource.slots",
  "resource.max_hold_minutes",
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
//   - `intake.source`, `effort` and `context` have no validation rule in this model at
//     all (the CLI knobs are capability data the backend owns, and intake's vocabulary
//     is nobody's business until slice E's `workflow_check` asks the engine directly);
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
});

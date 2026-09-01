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
  isRemoteLabel,
  KNOWN_BLOCK,
  REMOTE_LABEL_MAX,
  parseWorkflow,
  roleHintRequires,
  serializeWorkflow,
  DRIVER_DEFAULTS,
  isDriverOn,
  driverSectionHasComments,
  driverEnabledLineComment,
  serializeWorkflowPreserving,
  setDriverEnabled,
  removeDriverBlock,
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
  /** Longest accepted STRING (#1457). Distinct from `max`, which bounds a number. */
  maxLength?: number;
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
    case "driver":
      return ["version: 1", ...roster, "driver:", `  ${field}: ${yaml}`].join("\n") + "\n";
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
    case "driver":
      return at(w.driver as unknown as Record<string, unknown>);
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
 *  The `driver.*` rows arrived hand-built with #1869 (enable-toggle plus the six
 *  counters, bounds read from `POLICY_BOUNDS`) — the same shape `mergeQueueForm` is,
 *  and the same shape slice C will replace.
 *
 *  Slice C also has two manifest keys to HONOR, not merely to render (#880 review
 *  finding 4): `on_out_of_range` says whether a number's bound is refused by the engine
 *  (stop the submit) or silently clamped (accept and coerce), and `max_entries` on
 *  `workflow.resources` is a hard cap — an "add a resource" affordance that writes a
 *  33rd entry produces a file the engine refuses whole. Both are pinned against the
 *  engine in `src-tauri/tests/orchestration.rs`, so the data is trustworthy; consuming
 *  it is the renderer's half. */
const FIELDS_WITH_AN_EDITOR = new Set<string>([
  "driver.enabled",
  "driver.max_review_rounds",
  "driver.max_ci_attempts",
  "driver.max_rebase_attempts",
  "driver.lane_timeout_minutes",
  "driver.fix_timeout_minutes",
  "driver.drive_timeout_minutes",
]);

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
  // #1869 moved the driver fields OUT of this list: the pane now has an enable-toggle
  // (write rule in `setDriverEnabled` — off deletes a BARE block and keeps a configured
  // one as `enabled: false`; absent and `enabled: false` are the same state to the
  // engine) and bounded number fields for the six counters, so they are claimed in
  // `FIELDS_WITH_AN_EDITOR` above.
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

test("the pane's block-key set IS the manifest's, in both directions (#1457 review N1)", () => {
  // The other half of "(a) the parser knows every field". That test walks the
  // MANIFEST and checks the pane reads each one, so it catches a field the pane
  // forgot. It cannot catch the opposite — a key the PANE knows and the manifest
  // (and therefore the engine) does not — because it never reads the pane's set.
  //
  // That direction is the one `remote:` makes load-bearing (#1457). The engine
  // refuses a destination-shaped key by `deny_unknown_fields`, which is
  // default-deny; the pane's `KNOWN_BLOCK` is an allowlist, so a later PR adding
  // `hostname`, `addr`, `ssh_host`, `via` or `jump` to it would make the pane
  // report a file clean that the engine fails WHOLE — the launch then falls back
  // to the built-in roster with no finding to explain where the roster went.
  // A set equality needs no enumeration and cannot go stale.
  const manifestBlockKeys = new Set(manifest.sections.block!.fields.map((f) => f.name));
  const paneKeys = new Set(KNOWN_BLOCK);
  assert.deepEqual(
    [...paneKeys].filter((k) => !manifestBlockKeys.has(k)).sort(),
    [],
    "the pane reads a block key the schema does not declare — the engine will refuse the whole file over it"
  );
  assert.deepEqual(
    [...manifestBlockKeys].filter((k) => !paneKeys.has(k)).sort(),
    [],
    "the schema declares a block key the pane does not read — it would land in the unknown-key bag"
  );
  // The non-vacuity control: both sets are non-empty and really do contain the
  // key this test was written for, so a future refactor that empties either one
  // fails here rather than passing as two empty sets.
  assert.ok(paneKeys.size >= 12, `the pane's block-key set looks empty: ${paneKeys.size}`);
  assert.ok(paneKeys.has("remote") && manifestBlockKeys.has("remote"));
});

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
  // The non-vacuity control, mirroring the block-key test above: #1869 put seven
  // fields in `FIELDS_WITH_AN_EDITOR`, so the set is no longer empty — and a future
  // refactor that empties it (accidentally or "temporarily") must fail here rather
  // than pass as two mutually-empty sets.
  assert.ok(
    FIELDS_WITH_AN_EDITOR.size >= 7,
    `FIELDS_WITH_AN_EDITOR looks empty: ${FIELDS_WITH_AN_EDITOR.size}`
  );
  for (const id of ids) {
    if (id.startsWith("driver.")) {
      assert.ok(
        FIELDS_WITH_AN_EDITOR.has(id),
        `${id}: the driver block's fields are editable since #1869 — moving one back to pending without deleting its control is a silent retraction`
      );
    }
  }
});

// ---------- (c3) the driver toggle's write rule (#1869) ----------

test("the driver toggle's OFF deletes a BARE block and keeps a configured one (#1869 review 3)", () => {
  // Bare — nothing but `enabled`, and no comments in the file's prose about the
  // section: delete. Absent and `enabled: false` are the same state to the
  // engine, and deleting is the tidier of the two.
  const bare = parseWorkflow(
    "version: 1\nblocks:\n  - id: b\n    kind: worker\ndriver:\n  enabled: true\n"
  ).workflow;
  setDriverEnabled(bare, false);
  assert.equal(bare.driver, undefined, "a bare block is deleted whole");

  // Configured — the checkbox READS the `enabled:` line but writes the block, so
  // two clicks on it must never delete the counters. OFF writes the switch and
  // keeps everything else (round 2's original test asserted the delete here —
  // that rule was the data-loss path rev-final reproduced).
  const configured = parseWorkflow(
    "version: 1\nblocks:\n  - id: b\n    kind: worker\ndriver:\n  enabled: true\n  max_review_rounds: 2\n"
  ).workflow;
  setDriverEnabled(configured, false);
  assert.deepEqual(
    configured.driver,
    { enabled: false, max_review_rounds: 2 },
    "OFF on a configured block writes enabled: false and keeps the counters"
  );

  // The third input is the one the model cannot see: the file's own prose about
  // the section. A bare block the file comments on is kept too — deleting it
  // would delete that prose with it.
  const commented = parseWorkflow(
    "version: 1\nblocks:\n  - id: b\n    kind: worker\ndriver:\n  enabled: true\n"
  ).workflow;
  setDriverEnabled(commented, false, true);
  assert.deepEqual(
    commented.driver,
    { enabled: false },
    "OFF on a bare block whose section carries comments writes enabled: false"
  );
});

test("on-then-off on a configured block restores the file byte for byte (#1869 review 3)", () => {
  // The reviewer's data-loss case, pinned end to end: a block resting at
  // `enabled: false` with counters and comments renders UNCHECKED, and on-then-off
  // must land the file back where it started — not delete it. The splice reuses
  // the section's own lines with only the switch's line rewritten, so two clicks
  // round-trip to BYTE IDENTITY, comments and formatting included.
  const text =
    "version: 1\n" +
    "blocks:\n" +
    "  - id: b\n" +
    "    kind: worker\n" +
    "# the review loop - see docs/orchestration.md\n" +
    "driver:\n" +
    "  # INVARIANT 9's numbers, run tighter not looser\n" +
    "  enabled: false\n" +
    "  max_review_rounds: 2  # one round is enough here\n" +
    "";
  const w = parseWorkflow(text).workflow;
  const comments = driverSectionHasComments(text);
  assert.ok(comments, "positive control: the scan must SEE this fixture's comments");
  setDriverEnabled(w, true, comments);
  assert.deepEqual(w.driver, { enabled: true, max_review_rounds: 2 });
  const afterOn = serializeWorkflowPreserving(w, text);

  const w2 = parseWorkflow(afterOn).workflow;
  setDriverEnabled(w2, false, driverSectionHasComments(afterOn));
  const out = serializeWorkflowPreserving(w2, afterOn);
  assert.equal(
    out,
    text,
    `two clicks must restore the file byte for byte, not delete the block:\n${out}`
  );
  assert.deepEqual(
    parseWorkflow(out).workflow.driver,
    { enabled: false, max_review_rounds: 2 },
    "…and the model the restored file parses to is the one it started with"
  );
});

test("on-then-off on a LINELESS block leaves it declaring enabled: false — the documented exception (#1869 review 5)", () => {
  // The one state the byte-for-byte promise does NOT cover, disclosed in
  // docs/orchestration.md: a block with no `enabled:` line gains one the moment
  // the driver is switched on (there is no other way to turn it on), so the two
  // clicks leave it declaring `enabled: false` where the file had none. The
  // counters still survive — the data-loss rule is untouched by this exception.
  // This drives the PRESERVING serializer, the path the pane writes through and
  // the path the docs sentence is about (round 6: the round-5 version measured
  // the canonical emitter instead); the interior comment is what makes the
  // splice's INSERT branch distinguishable from a canonical regeneration, which
  // would drop it.
  const comment = "# a note the file's author wrote";
  const text =
    "version: 1\n" +
    "blocks:\n" +
    "  - id: b\n" +
    "    kind: worker\n" +
    "driver:\n" +
    `  ${comment}\n` +
    "  max_review_rounds: 2\n";
  const comments = driverSectionHasComments(text);
  assert.ok(comments, "positive control: the scan must SEE this fixture's comment");
  const w = parseWorkflow(text).workflow;
  setDriverEnabled(w, true, comments);
  const afterOn = serializeWorkflowPreserving(w, text);
  assert.match(
    afterOn,
    new RegExp(comment),
    "the splice's INSERT branch carries the comment through turning the driver on"
  );
  const w2 = parseWorkflow(afterOn).workflow;
  setDriverEnabled(w2, false, driverSectionHasComments(afterOn));
  const out = serializeWorkflowPreserving(w2, afterOn);
  assert.match(out, /enabled: false/, "the file gains the line it never had — that is the exception");
  assert.match(out, new RegExp(comment), "the comment survives both clicks");
  assert.match(out, /max_review_rounds: 2/, "…and so does the counter");
  assert.deepEqual(parseWorkflow(out).workflow.driver, { enabled: false, max_review_rounds: 2 });
});

test("the enabled splice's bail path really does drop interior comments — the disclosed residual (#1869 review 5)", () => {
  // The residual `spliceEnabledLine`'s doc states: on a shape the scan cannot
  // rewrite in place — here an `enabled: yes` line, which the reader refuses (the
  // pane flags it as a bad value) and the splice cannot spell — the section falls
  // back to canonical regeneration and the interior comment does not survive.
  // The model still round-trips correctly; the loss is prose, not data, and it is
  // the same trade every other section edit has always made.
  const comment = "# a note the file's author wrote";
  const text =
    "version: 1\n" +
    "blocks:\n" +
    "  - id: b\n" +
    "    kind: worker\n" +
    "driver:\n" +
    `  ${comment}\n` +
    "  enabled: yes\n" +
    "  max_review_rounds: 2\n";
  // The POSITIVE CONTROL first, on the same comment through the NON-bail path:
  // the splice really does carry this comment through a write, so the absence
  // asserted below measures the bail and not fixture drift (a `doesNotMatch`
  // passes just as well when the mechanism never ran — and this test has been
  // caught being vacuous under fixture drift once already, #1869 review 6).
  const cleanText = text.replace("enabled: yes", "enabled: true");
  const clean = parseWorkflow(cleanText).workflow;
  setDriverEnabled(clean, false);
  const cleanOut = serializeWorkflowPreserving(clean, cleanText);
  assert.match(
    cleanOut,
    new RegExp(comment),
    "positive control: the same comment survives the splice path"
  );
  assert.notEqual(cleanOut, cleanText, "…and the mechanism ran — the file was rewritten");

  // Now the bail itself: the rewrite cannot happen, the section regenerates.
  const w = parseWorkflow(text).workflow;
  assert.equal(w.driver?.enabled, undefined, "the reader refuses `yes` — the model has no enabled");
  setDriverEnabled(w, true);
  const out = serializeWorkflowPreserving(w, text);
  assert.notEqual(out, text, "the mechanism ran — the write landed");
  assert.doesNotMatch(
    out,
    new RegExp(comment),
    "the bail regenerates the section — the interior comment is gone, as disclosed"
  );
  assert.match(out, /enabled: true/, "…but the model's write lands");
  assert.deepEqual(
    parseWorkflow(out).workflow.driver,
    { enabled: true, max_review_rounds: 2 },
    "the data round-trips even where the prose does not"
  );
});

test("removeDriverBlock deletes a configured block whole — the escape hatch the toggle no longer provides (#1876 P1)", () => {
  // P1: the narrowed toggle preserves a configured block, so removal is its own
  // gesture — and the escape hatch for the forward-compat break: a `driver:`
  // block makes the file unloadable on an orrerix build old enough to refuse the
  // key (`RawWorkflow` is `deny_unknown_fields`, verified against v1.3.0-beta2).
  // The discard is whole and explicit: switch, counters, unknown keys, comments.
  const w = parseWorkflow(
    "version: 1\nblocks:\n  - id: b\n    kind: worker\ndriver:\n  # a comment\n  enabled: true\n  max_review_rounds: 2\n"
  ).workflow;
  removeDriverBlock(w);
  assert.equal(w.driver, undefined, "removal deletes the block whole — counters included");
  const out = serializeWorkflow(w);
  assert.doesNotMatch(out, /^driver:/m, "the emitted file carries no driver section");
  assert.equal(
    parseWorkflow(out).workflow.driver,
    undefined,
    "…and the write round-trips: the file is the one an old build can load"
  );
  removeDriverBlock(w);
  assert.equal(w.driver, undefined, "removing an absent block is a no-op, not a crash");
});

test("driverEnabledLineComment reads the enabled line's own comment through the splitter (#1876 P2)", () => {
  // The flip note's condition: the splice rewrites the enabled line's VALUE and
  // leaves its trailing comment exactly as written, so the note shows while that
  // comment is there, quoting it. The scanner is the preserving splitter (#233),
  // not a regex — a `#` inside a block scalar's body is content, and a body line
  // shaped like an `enabled:` field is not a field (it sits deeper than the
  // block's own).
  const roster = "version: 1\nblocks:\n  - id: b\n    kind: worker\n";
  assert.equal(
    driverEnabledLineComment(`${roster}driver:\n  enabled: false  # keep off until Q3\n`),
    "# keep off until Q3",
    "the annotated line's comment is the note's condition and its text"
  );
  assert.equal(driverEnabledLineComment(`${roster}driver:\n  enabled: true\n`), null);
  assert.equal(
    driverEnabledLineComment(`${roster}driver:\n  max_review_rounds: 2\n`),
    null,
    "no enabled line — nothing to annotate"
  );
  assert.equal(driverEnabledLineComment("version: 1\n"), null, "no driver block");
  assert.equal(
    driverEnabledLineComment(
      `${roster}driver:\n  mystery: |\n    enabled: false # body, not a field\n  enabled: true # real\n`
    ),
    "# real",
    "a block scalar's body is opaque: its `enabled:`-shaped line is not the field"
  );
  assert.equal(
    driverEnabledLineComment(`${roster}driver:\n  enabled: yes  # note\n`),
    null,
    "the bail shape: `yes` fails the splice's suffix guard, so the flip regenerates the " +
      "section and this comment would NOT survive — the note must not render (#1876 review 1)"
  );
  assert.equal(
    driverEnabledLineComment(" driver:\n  enabled: true # x\n"),
    null,
    "an unreadable shape invents no note — the note is advisory and may not lie"
  );
});

test("driverSectionHasComments reads the real splitter, not a second scanner (#1869 review 3)", () => {
  const roster = "version: 1\nblocks:\n  - id: b\n    kind: worker\n";
  // The introducing comment, an interior one, and a trailing one on the key line
  // are all the file's prose about the section.
  assert.equal(
    driverSectionHasComments(`${roster}# about the driver\ndriver:\n  enabled: true\n`),
    true
  );
  assert.equal(
    driverSectionHasComments(`${roster}driver:\n  # on, until the team says otherwise\n  enabled: true\n`),
    true
  );
  assert.equal(
    driverSectionHasComments(`${roster}driver: # off for now, keep the numbers\n  enabled: true\n`),
    true
  );
  assert.equal(
    driverSectionHasComments(`${roster}driver:\n  enabled: true\n`),
    false,
    "a clean block has no comments — the delete case needs this half to be reachable"
  );
  // A `#` inside an unknown key's block scalar is CONTENT, not commentary — the
  // preserving splitter's own rule (#233 B2). A naive second scanner would read it
  // as a comment and preserve a block the rule says to delete; this half pins that
  // the splitter, not a regex, is answering.
  assert.equal(
    driverSectionHasComments(`${roster}driver:\n  enabled: true\n  mystery: |\n    # not a comment\n`),
    false
  );
  // A shape the scan refuses to read is treated as commented: the OFF rule may
  // keep a block it could have deleted, but must never delete one it cannot see.
  assert.equal(driverSectionHasComments(" driver:\n  enabled: true\n"), true);
});

test("the driver toggle's ON from absent writes exactly { enabled: true }, and it round-trips (#1869)", () => {
  // No counters invented: every one defaults to INVARIANT 9's ceiling, so a first
  // run needs none — and the emitted YAML is what "the pane edits the YAML" means.
  const w = parseWorkflow("version: 1\nblocks:\n  - id: b\n    kind: worker\n").workflow;
  setDriverEnabled(w, true);
  assert.deepEqual(w.driver, { enabled: true }, "ON must write { enabled: true } and nothing else");
  const text = serializeWorkflow(w);
  assert.match(text, /^driver:\n  enabled: true\n/m, `the emitted file must carry the block:\n${text}`);
  assert.deepEqual(
    parseWorkflow(text).workflow.driver,
    { enabled: true },
    "the write must survive serialize → parse"
  );
});

test("the driver toggle's ON on a hand-written enabled: false keeps that file's other fields (#1869)", () => {
  // The merge-queue lesson (#1020 review, finding 4) at the other end of the toggle:
  // the counters are the human's lines. Flipping a declared-off block on must set
  // `enabled` and nothing else — replacing the block would wipe a value the human
  // wrote and the engine honors.
  const w = parseWorkflow(
    "version: 1\nblocks:\n  - id: b\n    kind: worker\ndriver:\n  enabled: false\n  max_review_rounds: 2\n"
  ).workflow;
  setDriverEnabled(w, true);
  assert.deepEqual(w.driver, { enabled: true, max_review_rounds: 2 });
});

test("the driver toggle's CHECKED state is the enabled line, not the block's presence (#1869 review 1)", () => {
  // The engine's `RawDriver.enabled` is `#[serde(default)] bool`: a PRESENT driver:
  // block without an `enabled:` line is OFF, exactly what the pre-form pane rendered
  // ("not declared - off (orrerix's default)"). The checkbox must show unchecked for
  // it — a human who commits believing a checked box would ship a driver that never
  // runs. This is the display half the write-rule tests cannot see, so it is pinned
  // here beside them, and composed with the write rule: ticking the unchecked box on
  // writes the line and keeps the counters.
  const absent = parseWorkflow("version: 1\nblocks:\n  - id: b\n    kind: worker\n").workflow;
  const lineless = parseWorkflow(
    "version: 1\nblocks:\n  - id: b\n    kind: worker\ndriver:\n  max_review_rounds: 2\n"
  ).workflow;
  const declaredOff = parseWorkflow(
    "version: 1\nblocks:\n  - id: b\n    kind: worker\ndriver:\n  enabled: false\n"
  ).workflow;
  const declaredOn = parseWorkflow(
    "version: 1\nblocks:\n  - id: b\n    kind: worker\ndriver:\n  enabled: true\n"
  ).workflow;
  assert.equal(isDriverOn(absent), false);
  assert.equal(
    isDriverOn(lineless),
    false,
    "a present block without the line is OFF — the engine's serde default decides, not the block"
  );
  assert.equal(isDriverOn(declaredOff), false);
  assert.equal(isDriverOn(declaredOn), true, "the declared true is the one ON state");
  setDriverEnabled(lineless, true);
  assert.deepEqual(
    lineless.driver,
    { enabled: true, max_review_rounds: 2 },
    "ticking the lineless block on writes the line and keeps the counters"
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
    // `role-hint-superseded` is exempt BY NAME, not by prefix: it is an advisory
    // about where a feature moved (#1161 D4), and the file it is raised on
    // parses and runs. Every other `role-hint-*` code is a refusal, and a
    // manifest value that draws one is a real disagreement.
    //
    // Scoped to the hint the advisory is ABOUT, not applied to every hint in the
    // loop (#1502 review N5): a future fourth hint that spuriously drew
    // `role-hint-superseded` would ride a blanket exemption through unnoticed.
    const exempt = hint.trim().toLowerCase() === "liaison" ? ["role-hint-superseded"] : [];
    assert.ok(
      !codes.some((c) => c.startsWith("role-hint") && !exempt.includes(c)),
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

test("the pane's remote-label cap IS the manifest's, which IS the engine's (#1457 review B3)", () => {
  // The third link of the engine -> manifest -> pane chain, for the one bound
  // that was outside it. `REMOTE_LABEL_MAX` carried a comment claiming this
  // mechanism while being enrolled in nothing: the engine stated no fact for
  // `block.remote`, so the manifest declared none, so the bidirectional Rust pin
  // had nothing to compare, so this constant was pinned to nothing at all.
  //
  // Both sides tested their cap against their OWN constant — the engine builds
  // its over-cap fixture from `MAX_SEGMENT_LEN + 1`, the pane from
  // `REMOTE_LABEL_MAX + 1` — so both stayed green while free to disagree.
  // `MAX_SEGMENT_LEN` is shared by four identifier families (#925), so a change
  // to it is a live possibility: raise it to 96 and the engine would accept a
  // 70-character label while the pane reported `remote-invalid-label` on a file
  // the engine loads clean.
  //
  // The Rust half (`the_workflow_schema_manifest_matches_the_engines_values_
  // defaults_and_bounds`, now pinning `maxLength` too) holds engine == manifest;
  // this holds manifest == pane. Neither operand here is this test's own.
  const declared = bound("block", "remote").maxLength;
  assert.equal(
    typeof declared,
    "number",
    "block.remote must declare a maxLength — without it the Rust pin has nothing to compare and this constant is pinned to nothing"
  );
  assert.equal(
    REMOTE_LABEL_MAX,
    declared,
    "the pane's cap and the manifest's disagree — the pane would paint a legal file red, or accept one the engine refuses"
  );

  // …and the pane's PREDICATE really uses that constant, rather than a literal
  // that happens to match it today. Without this the equality above could hold
  // while `isRemoteLabel` enforced something else entirely.
  assert.equal(isRemoteLabel("b".repeat(declared)), true, "a label exactly at the cap is legal");
  assert.equal(isRemoteLabel("b".repeat(declared + 1)), false, "one character over is not");
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
    // #1778 §2.3: the driver's counters are REFUSED (an error, like
    // `max_batch`), its backstops CLAMPED (a warning, like
    // `checks_timeout_minutes`) — one case per field, read against the
    // manifest's own on_out_of_range, so a field whose pane severity drifts
    // from its engine posture reddens by name.
    {
      section: "driver",
      field: "max_review_rounds",
      text: "version: 1\nblocks:\n  - id: b\n    kind: worker\n    cli: claude\ndriver:\n  max_review_rounds: 9\n",
    },
    {
      section: "driver",
      field: "max_ci_attempts",
      text: "version: 1\nblocks:\n  - id: b\n    kind: worker\n    cli: claude\ndriver:\n  max_ci_attempts: 9\n",
    },
    {
      section: "driver",
      field: "max_rebase_attempts",
      text: "version: 1\nblocks:\n  - id: b\n    kind: worker\n    cli: claude\ndriver:\n  max_rebase_attempts: 2\n",
    },
    {
      section: "driver",
      field: "lane_timeout_minutes",
      text: "version: 1\nblocks:\n  - id: b\n    kind: worker\n    cli: claude\ndriver:\n  lane_timeout_minutes: 9999\n",
    },
    {
      section: "driver",
      field: "fix_timeout_minutes",
      text: "version: 1\nblocks:\n  - id: b\n    kind: worker\n    cli: claude\ndriver:\n  fix_timeout_minutes: 9999\n",
    },
    {
      section: "driver",
      field: "drive_timeout_minutes",
      text: "version: 1\nblocks:\n  - id: b\n    kind: worker\n    cli: claude\ndriver:\n  drive_timeout_minutes: 9999\n",
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

test("a non-integer driver COUNTER is an error, not a silence (#1784 review 1)", () => {
  // Every driver field is a `u32` on the engine, so serde refuses `2.5`
  // exactly as it refuses an out-of-range value - and a check guarded by
  // `Number.isInteger` goes silent on it, blessing a file the engine refuses
  // at load (the pane-valid⇔loads guarantee). A counter, whose range also
  // refuses. The assertion is on SEVERITY, not the code alone: a clamp
  // warning for a value the engine refuses would be the same lie in a
  // friendlier tone.
  const findings = analyzeWorkflow(
    "version: 1\nblocks:\n  - id: b\n    kind: worker\n    cli: claude\ndriver:\n  max_review_rounds: 2.5\n"
  ).findings.filter((f) => f.severity === "error");
  assert.ok(
    findings.some((f) => f.code === "section-out-of-range" || f.code === "section-bad-value"),
    `a non-integer counter must be flagged as an ERROR, got ${JSON.stringify(findings.map((f) => [f.code, f.severity]))}`
  );
});

test("a non-integer driver BACKSTOP is an error too - the clamp never runs on a refused type (#1784 review 1)", () => {
  // The twin of the counter case, and the sharper one: the backstop's RANGE
  // clamps, so `2.5` (below 5) does draw an out-of-range WARNING from the
  // range check alone - but the engine refuses the TYPE before any clamp
  // runs, and a warning would bless a file that never loads. The error must
  // be there BESIDE the warning, which is why this asserts on the filtered
  // error list rather than on any finding at all.
  const findings = analyzeWorkflow(
    "version: 1\nblocks:\n  - id: b\n    kind: worker\n    cli: claude\ndriver:\n  fix_timeout_minutes: 2.5\n"
  ).findings.filter((f) => f.severity === "error");
  assert.ok(
    findings.some((f) => f.code === "section-out-of-range" || f.code === "section-bad-value"),
    `a non-integer backstop must be flagged as an ERROR, got ${JSON.stringify(findings.map((f) => [f.code, f.severity]))}`
  );
});

test("a typo'd key inside driver: is an unknown-key finding routed to the driver section (#1784 review 2)", () => {
  // Refused whole-file by the engine's `deny_unknown_fields` - the exact case
  // the round-4 red proves refused - so the pane must not render it valid.
  const findings = analyzeWorkflow(
    "version: 1\nblocks:\n  - id: b\n    kind: worker\n    cli: claude\ndriver:\n  max_revew_rounds: 3\n"
  ).findings;
  const unknown = findings.find((f) => f.code === "unknown-key" && f.section === "driver");
  assert.ok(unknown, "a typo'd driver key must be an unknown-key finding routed to the driver section");
  assert.ok(
    unknown!.message.includes("max_revew_rounds"),
    `the finding must name the key it found: ${unknown!.message}`
  );
});

test("a driver integer outside u32 is an error, never a clamp warning (#1784 review 2)", () => {
  // The third input class of the same defect: `-1` and `4294967296` ARE
  // integers, so they pass every `Number.isInteger` guard and land in the
  // range check - where the backstop's warning said "orrerix will clamp it"
  // about a file the engine REFUSES (u32 rejects the value before any clamp
  // runs). The fix routes the type class to the refusal error, and the test
  // forbids the word "clamp" in what the pane says about it.
  for (const [field, v] of [
    ["fix_timeout_minutes", "-1"],
    ["fix_timeout_minutes", "4294967296"],
    ["max_review_rounds", "-1"],
  ] as const) {
    const findings = analyzeWorkflow(
      `version: 1\nblocks:\n  - id: b\n    kind: worker\n    cli: claude\ndriver:\n  ${field}: ${v}\n`
    ).findings;
    const errors = findings.filter((f) => f.severity === "error");
    assert.ok(
      errors.length > 0,
      `driver.${field}: ${v} must draw an ERROR (the engine refuses it), got ${JSON.stringify(findings.map((f) => [f.severity, f.code]))}`
    );
    for (const f of errors) {
      // The lie under test is the PROMISE of a clamp ("orrerix will clamp it"),
      // not the word: the refusal message legitimately says the type is
      // rejected "before any clamp runs". Assert against the promise.
      assert.ok(
        !/will clamp/.test(f.message),
        `driver.${field}: ${v} - an error message must not promise a clamp: ${f.message}`
      );
    }
  }
});

test("the pane's driver defaults are the manifest's declared defaults (#1784)", () => {
  // The chrome renders `?? DRIVER_DEFAULTS.x` when the file omits a field; a
  // literal at the point of use is a number nothing can check (review
  // premortem 2: NOTIFY_EXPIRES_DEFAULT_MIN moves, the manifest follows, a
  // stale `?? 60` renders silently). This is the third link of the
  // engine → manifest → pane chain, the `DEFAULT_HOLD` pattern: the Rust pin
  // holds engine == manifest, this holds manifest == pane, field for field.
  const fields = manifest.sections.driver!.fields;
  for (const f of fields) {
    const pane = (DRIVER_DEFAULTS as Record<string, unknown>)[f.name];
    assert.ok(f.name in DRIVER_DEFAULTS, `driver.${f.name} has no pane default - the chrome cannot render an omitted field`);
    assert.equal(
      pane,
      f.default,
      `driver.${f.name}: the pane's rendered default drifted from the manifest's declared default`
    );
  }
});

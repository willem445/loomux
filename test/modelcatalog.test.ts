// The shared model catalog (#935 slice B) — the one answer to "which models may
// this CLI be pinned to?", consumed by the launcher's per-role selector and (next
// slice) the workflow pane's block editor.
//
// What these defend is not "the merge concatenates two arrays" but the ways a
// merged suggestion list silently costs a human the model they chose:
//
//   1. A ROLE DEFAULT THAT SURVIVES THE MERGE. `pickerSelection` falls to its
//      `custom…` branch for a value outside the list, so a merge that dropped a
//      curated default would open the form on a hand-typed id — looking exactly
//      like a human's own choice (`orchclis.test.ts` names the same failure for
//      the curated list alone).
//   2. THE INHERIT ROW STAYING FINDABLE, AND STAYING SELECTED. `INHERIT_MODEL` is
//      the empty id: the "send no --model at all" row that is the honest default
//      on opencode (#722). It is both the row a long probed list would bury and
//      the value a falsiness check would mistake for "nothing chosen" — and
//      either mistake ends with loomux pinning a model over the one the human
//      configured.
//   3. NO CURATED FALLBACK FOR A CLI WITH NO ROW. The block editor's CLI list is
//      wider than the launcher's (`gemini` is a WORKFLOW_CLIS member with no
//      ORCH_CLIS row), so a catalog that fell back to the first row would offer
//      claude's aliases as gemini models.
//   4. IDS VERBATIM. opencode's are `provider_id/model_id` and the `/` is part of
//      the id (#722) — a merge that normalized one would produce a model that
//      does not exist.

import { test } from "node:test";
import assert from "node:assert/strict";
import {
  CUSTOM_OPTION,
  ModelCatalog,
  blockModelOptions,
  curatedModels,
  mergeModelOptions,
  modelOptions,
  pickerSelection,
  type CliProbe,
} from "../src/modelcatalog.ts";
import { INHERIT_MODEL, ORCH_CLIS } from "../src/orchclis.ts";
import { ORCH_ROLES } from "../src/roster.ts";

const probe = (models: string[]): CliProbe => ({ available: true, models, error: null });

// ── the merge ───────────────────────────────────────────────────────────────

test("what the CLI reports leads; curated suggestions back it (#935)", () => {
  // The machine's own answer is current by construction and specific to this
  // install; the curated list is a suggestion written down in the repo months
  // ago. So the probe leads — but nothing curated is dropped, because a role
  // default is drawn from it.
  const merged = mergeModelOptions(["sonnet", "opus"], ["claude-sonnet-4.6", "claude-opus-4-8"]);
  assert.deepEqual(merged, ["claude-sonnet-4.6", "claude-opus-4-8", "sonnet", "opus"]);
});

test("an id in both lists appears once, in the position the CLI reported it", () => {
  const merged = mergeModelOptions(["sonnet", "opus", "haiku"], ["opus", "claude-sonnet-4.6"]);
  assert.deepEqual(merged, ["opus", "claude-sonnet-4.6", "sonnet", "haiku"]);
  assert.equal(merged.filter((m) => m === "opus").length, 1, "a duplicated id must render one row");
});

test("a CLI that reports nothing degrades to the curated list, not to an empty menu", () => {
  // A parse miss, an older build, a CLI that documents no models: the dropdown
  // still has to be usable, which is the whole reason curated lists exist.
  assert.deepEqual(mergeModelOptions(["sonnet", "opus"], []), ["sonnet", "opus"]);
  assert.deepEqual(modelOptions("claude", null), curatedModels("claude"));
  assert.deepEqual(
    modelOptions("claude", { available: false, models: [], error: "not on PATH" }),
    curatedModels("claude")
  );
});

test("every role default survives a merge that reports an unrelated catalog", () => {
  // The failure this pins: a probe returns 40 ids, the merge keeps only those,
  // and the role's default is no longer on the menu — so the picker opens on
  // `custom…` with the default prefilled, which reads as the human's own typing.
  const reported = ["a/one", "b/two", "c/three"];
  for (const cli of ORCH_CLIS) {
    const merged = mergeModelOptions(cli.models, reported);
    for (const { key } of ORCH_ROLES) {
      const dflt = cli.defaults[key];
      assert.ok(
        merged.includes(dflt),
        `${cli.id}'s ${key} default ${JSON.stringify(dflt)} must still be offered after a merge: ${JSON.stringify(merged)}`
      );
      // And it must still SELECT as itself rather than falling to the custom
      // branch — the two halves of the same failure.
      assert.equal(pickerSelection(merged, dflt).selected, dflt);
    }
  }
});

test("the inherit row is pinned first, never buried under a probed catalog (#722)", () => {
  const opencode = ORCH_CLIS.find((c) => c.id === "opencode")!;
  const reported = ["opencode/gpt-5.1-codex", "anthropic/claude-sonnet-4.6", "openai/gpt-5.2"];
  const merged = mergeModelOptions(opencode.models, reported);
  assert.equal(
    merged[0],
    INHERIT_MODEL,
    "the 'send no --model' row must lead — it is the row a human is least likely to " +
      "find at the bottom of a 40-entry menu, and the one loomux most wants them to keep"
  );
  assert.equal(merged.filter((m) => m === INHERIT_MODEL).length, 1, "pinning must not duplicate it");
  // Pinned, not sorted: everything else keeps the probe-leads order.
  assert.deepEqual(merged.slice(1, 1 + reported.length), reported);
});

test("a blank id from a probe never manufactures an inherit row (#722)", () => {
  // The empty id renders as "(none) — the model your own CLI config selects".
  // Offering it on a CLI whose curated row does not is loomux advertising an
  // inheritance the spawn path then overrides with `default_model`.
  const merged = mergeModelOptions(["sonnet", "opus"], ["", "  ", "claude-sonnet-4.6"]);
  assert.deepEqual(merged, ["claude-sonnet-4.6", "sonnet", "opus"]);
  assert.ok(!merged.includes(INHERIT_MODEL));
});

test("ids cross the merge verbatim — the `/` is part of an opencode id (#722)", () => {
  const raw = ["opencode/deepseek-v4-flash-free", "amazon-bedrock/arn:aws:bedrock:us-east-1::foo/bar"];
  const merged = mergeModelOptions([], raw);
  assert.deepEqual(merged, raw);
});

// ── curated lookup ──────────────────────────────────────────────────────────

test("a CLI with no curated row suggests nothing rather than another CLI's aliases", () => {
  // `gemini` is a WORKFLOW_CLIS member with no ORCH_CLIS row. `orchCliFor` would
  // hand back the FIRST row for it (that fallback exists for the launcher's own
  // Agent field, where the select is restricted to ORCH_CLIS ids) — here it would
  // offer `sonnet`/`opus`/`haiku`/`fable` as gemini models.
  assert.deepEqual(curatedModels("gemini"), []);
  assert.deepEqual(curatedModels("nope-not-a-cli"), []);
  assert.deepEqual(curatedModels("claude"), ORCH_CLIS.find((c) => c.id === "claude")!.models);
  // And it is a copy: a caller that sorts what it got must not reorder the menu
  // every other surface renders.
  const got = curatedModels("claude");
  got.push("mutated");
  assert.ok(!curatedModels("claude").includes("mutated"));
});

// ── picker state ────────────────────────────────────────────────────────────

test("inherit is a CHOICE, not an absence: an empty current selects the inherit row", () => {
  // The membership test must come before the "is it empty" test. With the order
  // reversed, a human's deliberate "inherit" reads as "nothing chosen" and falls
  // through to whatever happens to be first in the list — a real model silently
  // pinned over the one their own config selects (#722).
  //
  // The witness has the inherit row NOT first, and that is the whole point: fed
  // a merge's own output (where `mergeModelOptions` pins it at index 0) BOTH
  // orders answer `INHERIT_MODEL` and the specimen witnesses nothing. This
  // function is a general one over whatever list its caller hands it — including
  // the block editor's, and any future list — so it is pinned on the input that
  // can tell the two implementations apart.
  const models = ["opencode/gpt-5.1-codex", INHERIT_MODEL];
  const s = pickerSelection(models, INHERIT_MODEL);
  assert.equal(
    s.selected,
    INHERIT_MODEL,
    "an empty current that IS on the menu is the inherit row, not an unset field"
  );
  assert.equal(s.showCustom, false);
  // And the pinned-first list (what the launcher actually renders) agrees.
  assert.equal(pickerSelection([INHERIT_MODEL, "opencode/gpt-5.1-codex"], INHERIT_MODEL).selected, INHERIT_MODEL);
});

test("a known id selects its row; an unknown one opens the custom branch carrying it", () => {
  assert.deepEqual(pickerSelection(["sonnet", "opus"], "opus"), {
    selected: "opus",
    custom: "",
    showCustom: false,
  });
  // Bedrock ARNs, gateway deployment names, a model newer than this build: the
  // dropdown must stay a superset of the free text it replaced, never a filter.
  assert.deepEqual(pickerSelection(["sonnet", "opus"], "arn:aws:bedrock:us-east-1::foo"), {
    selected: CUSTOM_OPTION,
    custom: "arn:aws:bedrock:us-east-1::foo",
    showCustom: true,
  });
});

test("with no options at all the custom input IS the field, and is shown", () => {
  // A CLI with no curated row whose probe reported nothing (gemini today). The
  // pre-#935 picker left the input hidden here, so the menu's only entry was
  // `custom…` and choosing it was the only way to reach a box that should have
  // been there already — a dead end on the one CLI that needs the escape most.
  const s = pickerSelection([], "");
  assert.equal(s.selected, CUSTOM_OPTION);
  assert.equal(s.showCustom, true);
});

test("no current and no default falls to the first option, custom hidden", () => {
  assert.deepEqual(pickerSelection(["sonnet", "opus"], ""), {
    selected: "sonnet",
    custom: "",
    showCustom: false,
  });
});

// ── the block editor's list ─────────────────────────────────────────────────

test("a block that declares no model opens on the blank row, not on the first suggestion (#935)", () => {
  // `model:` is OPTIONAL in a workflow file, and leaving it out is a declared
  // state — `model_of` (workflow.rs) resolves it to `default_model(cli, kind)`.
  // The launcher has no equivalent (every role starts on a real default drawn
  // from the curated row), so its list carries no blank row for claude. Handed
  // that list unchanged, a block with no `model:` would open showing `sonnet` —
  // a choice nobody made — and there would be no way back to "leave it to
  // loomux" once anything was picked. That is a NARROWER field than the free
  // text this replaces, which is the one thing #935 may not do.
  const launcher = curatedModels("claude");
  assert.equal(pickerSelection(launcher, "").selected, "sonnet", "the launcher's own list falls to first");

  const block = blockModelOptions(launcher);
  assert.deepEqual(block, [INHERIT_MODEL, ...launcher]);
  assert.equal(pickerSelection(block, "").selected, INHERIT_MODEL);
  assert.equal(pickerSelection(block, "").showCustom, false);
});

test("a CLI whose curated row already carries the blank row gets no second one", () => {
  // opencode's row leads with INHERIT_MODEL (#722), and `mergeModelOptions` pins
  // it first. Two "(unset)" rows in one menu is a menu that looks broken.
  const opencode = curatedModels("opencode");
  assert.equal(opencode[0], INHERIT_MODEL, "the fixture this test is about");
  const block = blockModelOptions(opencode);
  assert.deepEqual(block, opencode);
  assert.equal(block.filter((m) => m === INHERIT_MODEL).length, 1);
});

test("a CLI with nothing to offer stays empty, so the picker opens on its custom input", () => {
  // `gemini` is a WORKFLOW_CLIS member with no ORCH_CLIS row, and today's
  // `--help` parser reports nothing for it either. A lone "(unset)" row in front
  // of an empty menu would be a dropdown whose only purpose is to be escaped
  // from; an empty custom box already means what the blank row means.
  assert.deepEqual(blockModelOptions(modelOptions("gemini", null)), []);
  assert.equal(pickerSelection(blockModelOptions([]), "").showCustom, true);
  // …and the moment the machine reports something, the menu appears WITH the row.
  const probed = modelOptions("gemini", probe(["pro", "flash"]));
  assert.deepEqual(blockModelOptions(probed), [INHERIT_MODEL, "pro", "flash"]);
});

test("blockModelOptions copies — a caller that reorders it must not reorder the catalog", () => {
  const opencode = curatedModels("opencode");
  blockModelOptions(opencode).push("mutated");
  assert.ok(!curatedModels("opencode").includes("mutated"));
});

// ── the probe seam ──────────────────────────────────────────────────────────

test("one probe per program, however many surfaces ask", async () => {
  let calls = 0;
  const catalog = new ModelCatalog(async (p) => {
    calls++;
    return probe([`${p}-model`]);
  });
  const [a, b] = await Promise.all([catalog.probe("claude"), catalog.probe("claude")]);
  await catalog.probe("claude");
  assert.equal(calls, 1, "the memo is on the in-flight promise, not just the result");
  assert.deepEqual(a.models, ["claude-model"]);
  assert.equal(a, b);
  await catalog.probe("copilot");
  assert.equal(calls, 2, "a different program is a different probe");
});

test("a probe that rejects degrades to unavailable — it never rejects onwards", async () => {
  // A rejected promise reaching a form's render path turns "we couldn't ask the
  // machine" into a broken field. The question is one loomux can live without an
  // answer to: curated suggestions plus `custom…` still render.
  const catalog = new ModelCatalog(() => Promise.reject(new Error("ipc down")));
  const p = await catalog.probe("claude");
  assert.equal(p.available, false);
  assert.deepEqual(p.models, []);
  assert.match(p.error ?? "", /ipc down/);
  assert.deepEqual(catalog.models("claude"), curatedModels("claude"), "and the menu still fills");
});

test("models() paints from curated before the probe lands, merged after", async () => {
  let release: (v: CliProbe) => void = () => {};
  const catalog = new ModelCatalog(() => new Promise<CliProbe>((r) => (release = r)));
  assert.equal(catalog.cached("claude"), null);
  assert.deepEqual(
    catalog.models("claude"),
    curatedModels("claude"),
    "a form paints on its first frame — it cannot await a probe with an 8s worst case"
  );
  const inflight = catalog.probe("claude");
  release(probe(["claude-sonnet-4.6"]));
  await inflight;
  assert.deepEqual(catalog.cached("claude")?.models, ["claude-sonnet-4.6"]);
  assert.deepEqual(catalog.models("claude"), [
    "claude-sonnet-4.6",
    ...curatedModels("claude"),
  ]);
});

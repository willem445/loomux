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
  probeFailure,
  worthKeeping,
  type CliProbe,
} from "../src/modelcatalog.ts";
import { detailFor, type ModelDetail } from "../src/modelcatalog.ts";
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

  // The claim, asserted FIRST so a red lands on it rather than on the list shape
  // that produces it: what a block with no `model:` OPENS ON.
  const block = blockModelOptions(launcher);
  assert.equal(
    pickerSelection(block, "").selected,
    INHERIT_MODEL,
    "a block with no model: must open on the blank row, not on a suggestion"
  );
  assert.equal(pickerSelection(block, "").showCustom, false);
  assert.deepEqual(block, [INHERIT_MODEL, ...launcher]);
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
  const gemini = blockModelOptions(modelOptions("gemini", null));
  assert.equal(
    pickerSelection(gemini, "").showCustom,
    true,
    "a lone blank row would hide the free-text box behind a menu with one escape in it"
  );
  assert.deepEqual(gemini, []);
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

// ── what the memo may keep (#935 slice C review, rev-507 finding 1) ──────────
//
// The catalog is now ONE app-wide instance rather than a field on each welcome
// form, so its memo has no natural expiry — a pane closing used to be one. A
// front memo that outlives the backend's own rule does not duplicate the cache,
// it makes it unreachable, and the rule it must not outlive is stated in
// cliprobe.rs: complete probes are cached for the app run, "failures and partial
// answers are NOT — a CLI installed while loomux is running must become
// launchable on the next probe".

test("a probe that failed is not kept: the next ask reaches a CLI installed since (#935)", async () => {
  // The reported regression, as a sequence: loomux starts with gemini not on
  // PATH, a surface probes, the human installs gemini. With the failure memoized
  // there is no next probe, and every surface reports it missing until loomux
  // restarts — the recovery cliprobe.rs goes out of its way to keep.
  let calls = 0;
  const catalog = new ModelCatalog(async () => {
    calls++;
    return calls === 1 ? probeFailure("'gemini' was not found on PATH") : probe(["pro", "flash"]);
  });
  const first = await catalog.probe("gemini");
  assert.equal(first.available, false);

  // The claim, asserted before the memo detail it follows from.
  const second = await catalog.probe("gemini");
  assert.equal(second.available, true, "the retry must surface the success, not the cached failure");
  assert.deepEqual(second.models, ["pro", "flash"]);
  assert.equal(calls, 2, "and it must have actually re-asked the machine");
});

test("a failure leaves nothing behind for the synchronous paths either", async () => {
  // `cached()` is what a form's first paint reads. A failure it can serve is a
  // failure that outlives the ask, which is the same defect seen from the other
  // side — and it would also make `models()` claim the machine had been asked.
  const catalog = new ModelCatalog(async () => probeFailure("'gemini' was not found on PATH"));
  await catalog.probe("gemini");
  assert.equal(catalog.cached("gemini"), null, "nothing kept, so nothing stale to serve");
  assert.deepEqual(catalog.models("claude"), curatedModels("claude"));
});

test("an available CLI that reported no list is not kept either — that is a PARTIAL answer", async () => {
  // opencode's enumerator failing (a network blip, a provider configured or
  // `opencode auth login` completed a minute later) returns available: true with
  // an empty list, which the backend declines to cache for exactly the reason it
  // declines to cache a failure. Completeness is deliberately not a wire field,
  // so "carries no list" is how the front memo reads the same fact.
  let calls = 0;
  const catalog = new ModelCatalog(async () => {
    calls++;
    return calls === 1 ? probe([]) : probe(["opencode/deepseek-v4-flash-free"]);
  });
  assert.deepEqual((await catalog.probe("opencode")).models, []);
  assert.deepEqual(
    (await catalog.probe("opencode")).models,
    ["opencode/deepseek-v4-flash-free"],
    "the list that landed after `opencode auth login` must reach the picker"
  );
  assert.equal(calls, 2);
});

test("worthKeeping is the backend's rule, read off the reply", () => {
  assert.equal(worthKeeping(probe(["sonnet"])), true);
  assert.equal(worthKeeping(probe([])), false, "available but nothing to say is a partial answer");
  assert.equal(worthKeeping(probeFailure("not found")), false);
  // A failure can never carry models, but the predicate must not depend on that.
  assert.equal(worthKeeping({ available: false, models: ["sonnet"], error: "x" }), false);
});

test("an answer worth keeping IS kept — the re-ask is bounded to the answers that aren't", async () => {
  // The other half of the rule: a real list must not turn into an IPC per paint.
  let calls = 0;
  const catalog = new ModelCatalog(async () => {
    calls++;
    return probe(["sonnet"]);
  });
  await catalog.probe("claude");
  await catalog.probe("claude");
  await catalog.probe("claude");
  assert.equal(calls, 1, "a complete probe is asked once for the app run");
});

test("concurrent askers share one probe even when the answer is not kept", async () => {
  // Dropping the memo on a failure must not turn N surfaces opening at once into
  // N subprocesses: the in-flight promise is still shared, and only a caller that
  // asks AFTER it resolved pays for a fresh one.
  let calls = 0;
  const catalog = new ModelCatalog(async () => {
    calls++;
    return probeFailure("not found");
  });
  await Promise.all([catalog.probe("gemini"), catalog.probe("gemini"), catalog.probe("gemini")]);
  assert.equal(calls, 1, "one flight, three askers");
  await catalog.probe("gemini");
  assert.equal(calls, 2, "and the ask after it resolved is the recovery path");
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

// ---- the list-models control probe (#993) ----------------------------------
//
// A second seam beside the probe, and the tests below defend the reason it is
// separate rather than folded in. `probe` reads what a CLI prints unprompted and
// every form fires it on open; `detect` SPAWNS the CLI to ask it a question, on
// a claim (that the question is free) that the vendor does not document. So the
// properties worth pinning are about restraint — nothing runs unasked, a second
// gesture does not become a second spawn — and about the answer never being
// allowed to narrow what the human can already pick.

const detail = (over: Partial<ModelDetail> & { id: string }): ModelDetail => ({
  resolvedId: "",
  name: "",
  description: "",
  supportsEffort: null,
  effortLevels: [],
  ...over,
});

test("a catalog with no detector wired reports nothing rather than failing", async () => {
  // The launcher's catalog predates this slice. A surface that asks a catalog
  // that cannot answer has to get a report, not a rejected promise reaching its
  // render path.
  const catalog = new ModelCatalog(async () => probe([]));
  const report = await catalog.detect("claude");
  assert.deepEqual(report.models, []);
  assert.match(report.error ?? "", /detector/);
  assert.equal(catalog.report("claude"), null);
});

test("no lookup rides along on a probe or a paint", async () => {
  // #993's restraint, narrowed by #1020 rather than dropped. The lookup can no
  // longer spawn an agent CLI, so a paint path may CALL it — but the paths that
  // never wanted it still must not issue one behind a caller's back. `models()`,
  // `cached()`, `detail()`, `report()` and `probe()` all answer from what is
  // already in hand, and a form that only paints costs no IPC at all.
  let detects = 0;
  const catalog = new ModelCatalog(
    async () => probe(["claude-sonnet-4.6"]),
    async () => {
      detects += 1;
      return { models: [detail({ id: "opus" })], error: null };
    }
  );
  catalog.models("claude");
  catalog.detail("claude", "opus");
  catalog.report("claude");
  await catalog.probe("claude");
  catalog.models("claude");
  assert.equal(detects, 0, "reading what is already known must not reach the backend");
  await catalog.detect("claude");
  assert.equal(detects, 1, "only `detect` looks anything up");
});

test("a picker opening asks once per CLI, and the memo answers every reopen", async () => {
  // **The bound that replaced the click** (#1020). Under #993 a re-ask cost a
  // human gesture, which was the rate limit; the callers are paints now, and a
  // form that re-renders would issue one per paint without this.
  let detects = 0;
  const catalog = new ModelCatalog(
    async () => probe([]),
    async () => {
      detects += 1;
      return { models: [detail({ id: "opus" })], error: null };
    }
  );
  // Two pickers painting at once — the launcher opens four role rows together.
  await Promise.all([catalog.detect("claude"), catalog.detect("claude")]);
  assert.equal(detects, 1, "concurrent paints share one flight");
  // …and the form re-rendering, over and over, the way selecting blocks does.
  await catalog.detect("claude");
  await catalog.detect("claude");
  assert.equal(detects, 1, "a repaint reads the memo — it must never re-issue");
  assert.deepEqual((await catalog.detect("claude")).models.map((m) => m.id), ["opus"], "and it is the same answer");
});

test("a barren answer leaves report() empty but is still not asked twice", async () => {
  // The asymmetry #1020 introduces, and the reason for two maps rather than one.
  //
  // KEEPING the answer is still refused: `report()` stays null so every surface
  // falls back to its seed, exactly as before. ASKING again is now refused too,
  // which under #993 it was not — because the thing that would have to change
  // its mind is the backend's startup sweep, and the sweep PUSHES
  // (`acceptReport`). Re-issuing per paint would buy nothing and cost an IPC
  // per render.
  let detects = 0;
  const catalog = new ModelCatalog(
    async () => probe([]),
    async () => {
      detects += 1;
      return { models: [], error: "no models detected for this CLI yet" };
    }
  );
  const first = await catalog.detect("claude");
  assert.deepEqual(first.models, []);
  assert.equal(catalog.report("claude"), null, "a barren answer leaves nothing behind for a surface to show");
  await catalog.detect("claude");
  assert.equal(detects, 1, "…and is not chased on the next paint either");
});

test("a pushed report reaches a catalog that already looked and found nothing", async () => {
  // The push half of #1020, and the case that makes it load-bearing: a picker
  // painted while the sweep was still running looked, was told "nothing yet",
  // and memoized that. Without `acceptReport` its dropdown would keep the
  // curated seed for the life of the app — which is what the human sees as
  // detection being broken.
  let detects = 0;
  const catalog = new ModelCatalog(
    async () => probe([]),
    async () => {
      detects += 1;
      return { models: [], error: "no models detected for this CLI yet" };
    }
  );
  await catalog.detect("claude");
  assert.equal(catalog.report("claude"), null);

  const changed = catalog.acceptReport("claude", { models: [detail({ id: "opus" })], error: null });
  assert.equal(changed, true, "a report that carries models changed something");
  assert.deepEqual(catalog.report("claude")?.models.map((m) => m.id), ["opus"]);
  assert.ok(catalog.models("claude").includes("opus"), "and it reaches the menu the picker paints from");

  // The push also settles the pull, so a picker opening afterwards reads it
  // rather than spending an IPC on an answer already in hand.
  assert.deepEqual((await catalog.detect("claude")).models.map((m) => m.id), ["opus"]);
  assert.equal(detects, 1, "the pushed answer is the answer — no second lookup");
});

test("a pushed report that carries nothing never overwrites one that does", async () => {
  // The ordering hazard the push introduces: the sweep can emit for a CLI it
  // found nothing for, and that must not land on top of a real answer — nor
  // register as a change worth repainting for.
  const catalog = new ModelCatalog(async () => probe([]));
  catalog.acceptReport("claude", { models: [detail({ id: "opus" })], error: null });
  const changed = catalog.acceptReport("claude", { models: [], error: "not installed" });
  assert.equal(changed, false, "an empty report changes nothing, so no surface owes a repaint");
  assert.deepEqual(
    catalog.report("claude")?.models.map((m) => m.id),
    ["opus"],
    "and the real answer survives it — a later emit must not erase an earlier one"
  );
});

test("the two routes racing does not repaint a form twice", async () => {
  // The ordinary case, not the odd one: the lookup and the push are two
  // deliveries of ONE sweep answer, and both can land. The second must not
  // rebuild a dropdown that already shows it — a deferred rebuild fires on
  // blur, so a redundant one can land under a caret the human has moved into.
  //
  // The rule is a statement about the PRODUCER: the sweep asks each CLI once
  // per app run, so a second answer for the same CLI is never new information.
  let fired = 0;
  const catalog = new ModelCatalog(
    async () => probe([]),
    async () => ({ models: [detail({ id: "opus" })], error: null })
  );
  catalog.onReport(() => {
    fired += 1;
    return true;
  });
  // The lookup wins the race…
  await catalog.detect("claude");
  assert.deepEqual(catalog.report("claude")?.models.map((m) => m.id), ["opus"]);
  // …and the event arrives afterwards carrying the same answer.
  const changed = catalog.acceptReport("claude", { models: [detail({ id: "opus" })], error: null });
  assert.equal(changed, false, "the same answer by the other route is not a change");
  assert.equal(fired, 0, "so nothing is asked to repaint what it already painted");
});

test("a listener that reports itself gone is dropped rather than kept forever", async () => {
  // The lifecycle contract of `onReport`, and why the callback returns a
  // boolean. Neither host has a teardown this module can rely on — a launcher
  // form is discarded with its pane — so a subscription that outlived its DOM
  // would repaint detached controls for the rest of the app run.
  const catalog = new ModelCatalog(async () => probe([]));
  const alive: string[] = [];
  const dead: string[] = [];
  catalog.onReport((p) => {
    alive.push(p);
    return true;
  });
  catalog.onReport((p) => {
    dead.push(p);
    return false;
  });
  const unsubscribe = catalog.onReport(() => {
    assert.fail("an explicitly unsubscribed listener must never fire");
  });
  unsubscribe();

  catalog.acceptReport("claude", { models: [detail({ id: "opus" })], error: null });
  catalog.acceptReport("copilot", { models: [detail({ id: "gpt-5.2" })], error: null });
  assert.deepEqual(alive, ["claude", "copilot"], "a live listener hears every report");
  assert.deepEqual(dead, ["claude"], "one that said it was gone hears exactly one and is then dropped");
});

test("a listener never fires for a report that changed nothing", async () => {
  // Otherwise every host would have to re-derive "have I already seen this?"
  // before repainting — and repainting on an empty report is how a form rebuilds
  // itself under a human's caret for no reason at all.
  const catalog = new ModelCatalog(async () => probe([]));
  let fired = 0;
  catalog.onReport(() => {
    fired += 1;
    return true;
  });
  catalog.acceptReport("claude", { models: [], error: "not installed" });
  assert.equal(fired, 0, "nothing changed, so nobody is asked to repaint");
  catalog.acceptReport("claude", { models: [detail({ id: "opus" })], error: null });
  assert.equal(fired, 1);
});

test("a rejected detection degrades instead of reaching a render path", async () => {
  const catalog = new ModelCatalog(async () => probe([]), async () => {
    throw new Error("ipc exploded");
  });
  const report = await catalog.detect("claude");
  assert.deepEqual(report.models, []);
  assert.match(report.error ?? "", /ipc exploded/);
});

test("detection adds rows to the menu and never removes one", async () => {
  // The property `orchclis.test.ts` names, carried to the new source: a role's
  // default is drawn from the curated list, and a default that fell off the menu
  // opens the picker on its `custom…` branch — which reads as a human's typing.
  const catalog = new ModelCatalog(
    async () => probe([]),
    async () => ({ models: [detail({ id: "opus[1m]" }), detail({ id: "haiku" })], error: null })
  );
  const before = catalog.models("claude");
  await catalog.detect("claude");
  const after = catalog.models("claude");
  for (const id of before) assert.ok(after.includes(id), `${id} fell off the menu`);
  assert.ok(after.includes("opus[1m]"), "the CLI's own row is offered");
  assert.equal(after[0], "opus[1m]", "what the machine reported leads what this repo suggested");
});

test("detail() answers only about the model the CLI actually named", async () => {
  const catalog = new ModelCatalog(
    async () => probe([]),
    async () => ({ models: [detail({ id: "opus", supportsEffort: true, effortLevels: ["low", "max"] })], error: null })
  );
  assert.equal(catalog.detail("claude", "opus"), null, "nothing until a human asks");
  await catalog.detect("claude");
  assert.deepEqual(catalog.detail("claude", "opus")?.effortLevels, ["low", "max"]);
  assert.equal(catalog.detail("claude", "sonnet"), null, "a model the reply did not mention stays unknown");
  assert.equal(catalog.detail("copilot", "opus"), null, "and a CLI nobody detected stays unknown too");
});

test("a [1m] pick finds the base model's reported row", () => {
  // The suffix selects a context window, not a different model, and the CLI
  // enumerates base ids. Missing here would silently drop the effort levels the
  // plain variant shows, which reads as the suffix having disabled something.
  const models = [detail({ id: "sonnet", supportsEffort: true, effortLevels: ["low", "high"] })];
  assert.equal(detailFor(models, "sonnet[1m]")?.id, "sonnet");
  assert.equal(detailFor(models, "SONNET")?.id, "sonnet");
  assert.equal(detailFor(models, "claude-sonnet-4-5"), null, "never widened to a family — that is a different model");
  assert.equal(detailFor(models, ""), null);
});

test("an id the CLI itself reported with a suffix still matches verbatim first", () => {
  const models = [detail({ id: "opus[1m]", name: "Opus (1M context)" }), detail({ id: "opus", name: "Opus" })];
  assert.equal(detailFor(models, "opus[1m]")?.name, "Opus (1M context)", "the exact row wins before any widening");
  assert.equal(detailFor(models, "opus")?.name, "Opus");
});

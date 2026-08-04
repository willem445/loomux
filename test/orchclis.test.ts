// The launcher's orchestrator-mode CLI catalog (#4 per-role picks, #722 opencode).
//
// What these defend is not "the list has three rows" but the three ways a curated
// suggestion list can cost a human real money or a failed launch:
//
//   1. A ROLE DEFAULT THAT ISN'T OFFERED. `ModelPicker.setOptions` falls to its
//      `custom…` branch for a value outside the list, so a default nobody offers
//      opens the form on a hand-typed id — looking like a human's own choice.
//   2. A DEFAULT LOOMUX INVENTED. On a CLI with no vendor-neutral alias, any id
//      loomux picks silently overrides the model the human already configured.
//      That is why the backend's `default_model("opencode", _)` is empty, and the
//      launcher has to agree with it or the form advertises an inheritance it
//      then overrides.
//   3. AN ID THE LAUNCHER CANNOT PROBE. Orchestrator mode probes the CLI *id*
//      as a program name (`currentProgram`, `orchProgramsToCheck`), so a row
//      whose id is not a launchable program's command warns about the wrong
//      thing — or never warns at all, and the group fails at spawn instead.

import { test } from "node:test";
import assert from "node:assert/strict";
import { ORCH_CLIS, orchCliFor, INHERIT_MODEL } from "../src/orchclis.ts";
import { AGENTS } from "../src/agents.ts";
import { ORCH_ROLES } from "../src/roster.ts";

test("opencode is a CLI a group can be launched on (#722)", () => {
  const ids = ORCH_CLIS.map((c) => c.id);
  assert.ok(ids.includes("opencode"), `opencode must be offered in orchestrator mode: ${ids.join(", ")}`);
  // And it is not the fallback row: `orchCliFor` returns ORCH_CLIS[0] for
  // anything it doesn't know, so a typo'd id must not silently become opencode.
  assert.notEqual(ORCH_CLIS[0]!.id, "opencode");
  assert.equal(orchCliFor("opencode").id, "opencode");
  assert.equal(orchCliFor("nope-not-a-cli").id, ORCH_CLIS[0]!.id);
});

test("opencode pins no model on any role — every role inherits the human's own (#722)", () => {
  // The backend deliberately returns "" from `default_model("opencode", _)`:
  // opencode's ids are `provider_id/model_id` across dozens of providers, so
  // there is no vendor-neutral alias to default to, and a default would be both
  // a hardcoded model table (#329) and a silent override of the human's
  // `opencode.json`. A default here would put the launcher at odds with that.
  const oc = orchCliFor("opencode");
  for (const { key } of ORCH_ROLES) {
    assert.equal(
      oc.defaults[key],
      INHERIT_MODEL,
      `opencode must not pin a model for ${key} — loomux has no alias to pick and the human already chose one`
    );
  }
  // The inherit entry is a real, FIRST menu row, not an absence: an untouched
  // form has to be able to send "no --model", and `setOptions` selects the first
  // entry when the default is empty.
  assert.equal(oc.models[0], INHERIT_MODEL, "the inherit entry must be the one an untouched form lands on");
});

test("the curated opencode ids keep their provider prefix, `/` intact (#722)", () => {
  const oc = orchCliFor("opencode");
  // The free Zen model this issue exists for, exactly as the vendor spells it —
  // provider key `opencode`, not `zen`. Dropping the `/` yields
  // `opencodedeepseek-v4-flash-free`, a model that does not exist, which is the
  // latent bug slice A's `sanitize_model` widening fixed; nothing on the
  // launcher path may re-introduce it.
  assert.ok(
    oc.models.includes("opencode/deepseek-v4-flash-free"),
    `the curated list must offer the docs-verified Zen id: ${oc.models.join(", ")}`
  );
  for (const m of oc.models) {
    if (m === INHERIT_MODEL) continue;
    assert.match(m, /^[a-z0-9][a-z0-9._-]*\/[^/\s]+$/, `an opencode model id is provider_id/model_id: ${m}`);
  }
});

test("every role default is something the picker actually offers", () => {
  // Otherwise the form opens on `custom…` with a prefilled id that looks like
  // the human typed it. Empty is always legal — it means "send no --model".
  for (const cli of ORCH_CLIS) {
    for (const { key } of ORCH_ROLES) {
      const d = cli.defaults[key];
      assert.ok(
        d === INHERIT_MODEL || cli.models.includes(d),
        `${cli.id}'s ${key} default "${d}" is not in its own model list (${cli.models.join(", ")})`
      );
    }
  }
});

test("every orchestrator CLI id is a program the launcher can probe on PATH", () => {
  // Orchestrator mode passes the CLI id straight to `probe_agent_cli` as the
  // program name, and refuses the launch when it isn't found. A row whose id is
  // not some AGENTS entry's command would make that check probe a program name
  // nothing launches.
  for (const cli of ORCH_CLIS) {
    const agent = AGENTS.find((a) => a.command.split(/\s+/)[0] === cli.id);
    assert.ok(agent, `no launchable agent runs "${cli.id}" — the PATH probe would look for the wrong program`);
  }
});

test("every role has a default on every CLI — a role can never be left unresolved", () => {
  for (const cli of ORCH_CLIS) {
    for (const { key } of ORCH_ROLES) {
      assert.equal(typeof cli.defaults[key], "string", `${cli.id} has no default for ${key}`);
    }
  }
});

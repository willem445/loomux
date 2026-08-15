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

// ── copilot's row is a catalog, not a shortcut (#1020) ──────────────────────
//
// Copilot is the one CLI nothing on the machine will answer for: no `ENUMERATORS`
// row, no `PROTOCOLS` row, and a `--help` that no longer enumerates models under
// `--model`, so `parse_models_from_help` comes back empty and the merge has
// nothing to put in front of these. The curated list IS the menu, which makes two
// ways of getting it wrong worth pinning — and neither is "the list has 28 rows",
// because the human may re-issue that list at any time.

/** The vendor family an id belongs to — the leading alphabetic run, so
 *  `gpt-5.6-sol`, `claude-opus-4.8-fast` and `kimi-k3` collapse to `gpt`,
 *  `claude` and `kimi`. Deliberately derived rather than listed: a test carrying
 *  its own copy of the families would just be the model table again. */
const familyOf = (id: string): string => /^[a-z]+/.exec(id)?.[0] ?? "";

test("copilot's menu spans the vendors it resells, not one account's entitlements (#1020)", () => {
  const cp = orchCliFor("copilot");
  const families = new Set(cp.models.filter((m) => m !== "auto").map(familyOf));
  families.delete("");
  // The discriminating assertion, and it discriminates against BOTH failures
  // this row exists to prevent. A five-id shortcut reaches two families; the
  // per-account "Supported models" set copilot also reports reaches one (it is
  // claude-only on the install that produced it) — and embedding THAT would bake
  // one machine's plan into product code, which constraint 8 forbids. Copilot
  // resells several vendors, so a menu that names only one or two is not the
  // product's catalog whatever else is true of it.
  assert.ok(
    families.size >= 5,
    `copilot's curated menu covers only ${[...families].join(", ")} — that is a subset, not copilot's catalog`
  );
  // A witness per non-obvious family: these are the ids no other source can
  // supply (nothing probes copilot), so if the row is ever cut back to the
  // claude/gpt shortcut it was, these are what disappear.
  for (const id of ["gemini-3.7-flash", "grok-4.5", "kimi-k3"]) {
    assert.ok(cp.models.includes(id), `copilot resells ${familyOf(id)}, but the menu offers no ${id}`);
  }
});

test("copilot opens on `auto`, and offers no blank row (#1020)", () => {
  const cp = orchCliFor("copilot");
  // `auto` is copilot's own pick-for-me and the default on every role, so it has
  // to be the row an untouched form lands on: `setOptions` selects the first
  // entry when nothing else is chosen, and a catalog this long would otherwise
  // open on whichever id happened to lead it.
  assert.equal(cp.models[0], "auto", "copilot's pick-for-me must lead its own menu");
  for (const m of cp.models) {
    // No INHERIT_MODEL row, unlike opencode. An empty id renders as "the model
    // your own CLI config selects" — an inheritance the spawn path then
    // overrides, because `default_model("copilot", _)` is `auto`, not empty.
    assert.notEqual(m, INHERIT_MODEL, "copilot has a real default, so a blank row would advertise a false inheritance");
    // Copilot ids are bare vendor ids. A `/` is opencode's provider prefix and a
    // space would be a mis-pasted catalog line; either reaches `--model` verbatim
    // and fails at spawn.
    assert.match(m, /^[a-z0-9][a-z0-9.-]*$/, `not a copilot model id: ${JSON.stringify(m)}`);
  }
  assert.equal(new Set(cp.models).size, cp.models.length, "a duplicated id is a duplicated dropdown row");
});

test("every role has a default on every CLI — a role can never be left unresolved", () => {
  for (const cli of ORCH_CLIS) {
    for (const { key } of ORCH_ROLES) {
      assert.equal(typeof cli.defaults[key], "string", `${cli.id} has no default for ${key}`);
    }
  }
});

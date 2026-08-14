// Per-knob availability for the launcher's model selector (#687 slice B).
//
// The rule this module exists to keep is that loomux never offers a knob it
// cannot deliver, and never SILENTLY drops one either: an unavailable knob is
// disabled with the vendor's own reason, and its value can never reach the
// spawn payload. Three ways a knob is unavailable, and all three are here:
//
//   the CLI has no seam      — copilot's effort lives in ~/.copilot/settings.json
//   loomux never evaluated it — an unknown CLI claims nothing
//   the MODEL has no such form — `haiku[1m]` is not a Claude alias at all
//
// That last one is the #709 review's carried finding: `context:` was gated on
// the CLI and not on the model, so `haiku` + `1m` composed `--model haiku[1m]`,
// an alias the vendor docs do not define. Sources (fetched 2026-08-02, per the
// `agent-cli-reference` discipline): Claude Code model-config §Model aliases
// lists `sonnet[1m]`, `opus[1m]` (and §opusplan `opusplan[1m]`) and no haiku or
// fable form; §Extended context — "Fable 5, Sonnet 5, Opus 4.6 and later, and
// Sonnet 4.6 support a 1 million token context window", "You can also use the
// `[1m]` suffix with model aliases or full model names", and the rule itself:
// "Only append `[1m]` when the underlying model supports 1M context."

import { test } from "node:test";
import assert from "node:assert/strict";
import { readdirSync, readFileSync, statSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { knobState, knobValue, type CliKnobs } from "../src/selectorknobs.ts";

/** The `agent_cli_knobs` reply for claude, verbatim from `CLI_CAPS`. */
const CLAUDE: CliKnobs = {
  cli: "claude",
  known: true,
  effort: {
    values: ["low", "medium", "high", "xhigh", "max"],
    note: "--effort <level> is a session-scoped flag; a model that lacks a level falls back to the highest one it supports at or below it",
  },
  context: {
    values: ["1m"],
    note: "the [1m] model-alias suffix (sonnet[1m]) — access is plan- and credit-gated, so a tier the account cannot serve fails at the CLI, visibly in the pane",
  },
};

const COPILOT: CliKnobs = {
  cli: "copilot",
  known: true,
  effort: {
    values: [],
    note: "copilot reads effortLevel from ~/.copilot/settings.json — its programmatic reference documents no flag and no environment variable, and loomux never writes a user's global settings file",
  },
  context: {
    values: [],
    note: "copilot's context window is an interactive-only control (/context) with no argv or settings equivalent",
  },
};

/** What the backend returns for a CLI it has never evaluated: `known: false`,
 *  both sets empty, and — deliberately — no note to quote. */
const UNKNOWN: CliKnobs = {
  cli: "zed",
  known: false,
  effort: { values: [], note: "" },
  context: { values: [], note: "" },
};

test("claude offers both knobs on a model whose [1m] form the docs define", () => {
  const s = knobState(CLAUDE, "claude", "sonnet");
  assert.equal(s.effort.enabled, true);
  assert.deepEqual(s.effort.values, ["low", "medium", "high", "xhigh", "max"]);
  assert.equal(s.effort.reason, "");
  assert.equal(s.context.enabled, true);
  assert.deepEqual(s.context.values, ["1m"]);
  assert.equal(s.context.reason, "");
});

test("a CLI with no seam is disabled carrying ITS OWN reason, not 'unsupported'", () => {
  // The whole point of the backend shipping a note beside every empty value set:
  // a disabled control that states the vendor fact reads as a fact, while a bare
  // "unsupported" reads as loomux having forgotten.
  const s = knobState(COPILOT, "copilot", "claude-sonnet-4.6");
  assert.equal(s.effort.enabled, false);
  assert.deepEqual(s.effort.values, []);
  assert.equal(s.effort.reason, COPILOT.effort.note);
  assert.match(s.effort.reason, /~\/\.copilot\/settings\.json/);
  assert.equal(s.context.enabled, false);
  assert.equal(s.context.reason, COPILOT.context.note);
  assert.match(s.context.reason, /\/context/);
});

test("a CLI loomux has never evaluated claims nothing, and says so", () => {
  const s = knobState(UNKNOWN, "zed", "whatever");
  assert.equal(s.effort.enabled, false);
  assert.equal(s.context.enabled, false);
  // No note to quote — but a disabled control with an EMPTY reason is the
  // silent-drop failure this module exists to prevent.
  assert.notEqual(s.effort.reason, "");
  assert.notEqual(s.context.reason, "");
  assert.match(s.effort.reason, /zed/);
});

test("caps that have not arrived (or belong to another CLI) disable, never assume", () => {
  const none = knobState(null, "claude", "sonnet");
  assert.equal(none.effort.enabled, false);
  assert.equal(none.context.enabled, false);
  assert.notEqual(none.effort.reason, "");
  // A caps record for a DIFFERENT cli is a stale async reply landing after the
  // human changed the picker. Reading it would offer claude's five effort levels
  // on a copilot row.
  const stale = knobState(CLAUDE, "copilot", "auto");
  assert.equal(stale.effort.enabled, false);
  assert.equal(stale.context.enabled, false);
});

test("context is gated on the MODEL too — haiku has no [1m] form (#709 carried finding)", () => {
  const s = knobState(CLAUDE, "claude", "haiku");
  assert.equal(s.context.enabled, false);
  assert.deepEqual(s.context.values, []);
  assert.match(s.context.reason, /haiku\[1m\]/);
  // Effort is NOT gated with it: per model-config §Adjust effort level a model
  // that lacks a level falls back to the highest it supports, so every level is
  // safe to emit on every claude model.
  assert.equal(s.effort.enabled, true);
});

test("the model gate reads the FAMILY, so a full haiku model name is caught too", () => {
  // `claude-haiku-4-5[1m]` is the same defect as `haiku[1m]` — a full model name
  // is not a licence, the underlying model still has to support the window.
  assert.equal(knobState(CLAUDE, "claude", "claude-haiku-4-5").context.enabled, false);
  assert.equal(knobState(CLAUDE, "claude", "claude-opus-4-8").context.enabled, true);
  assert.equal(knobState(CLAUDE, "claude", "claude-sonnet-4.6").context.enabled, true);
});

test("aliases with no documented [1m] form are disabled for their own reasons", () => {
  // fable: §Extended context says Fable 5 already runs with the 1M window, and
  // the alias table has no `fable[1m]` row.
  const fable = knobState(CLAUDE, "claude", "fable");
  assert.equal(fable.context.enabled, false);
  assert.match(fable.context.reason, /Fable 5/);
  // best/default resolve per account, so there is no alias to suffix.
  assert.equal(knobState(CLAUDE, "claude", "best").context.enabled, false);
  assert.equal(knobState(CLAUDE, "claude", "default").context.enabled, false);
  // opusplan IS documented (`opusplan[1m]`, model-config §opusplan).
  assert.equal(knobState(CLAUDE, "claude", "opusplan").context.enabled, true);
});

test("an unrecognized model id fails OPEN — loomux does not pre-judge a provider id", () => {
  // A Bedrock inference profile, a gateway deployment name, a custom option: the
  // docs say the suffix works "with model aliases or full model names", and on
  // third-party providers the suffix is exactly how the 1M window is selected.
  // Refusing what we merely don't recognize would remove a real capability;
  // the failure direction for an unknown id is the CLI's own error, visibly in
  // the pane (the same posture slice A took on plan-gated entitlements).
  assert.equal(knobState(CLAUDE, "claude", "us.anthropic.claude-sonnet-4-6-v1:0").context.enabled, true);
  // Empty model = the CLI's own default, which is not ours to second-guess.
  assert.equal(knobState(CLAUDE, "claude", "").context.enabled, true);
});

test("the model gate is case- and whitespace-insensitive, like the backend's clamp", () => {
  assert.equal(knobState(CLAUDE, "claude", " Haiku ").context.enabled, false);
  assert.equal(knobState(CLAUDE, "claude", "SONNET").context.enabled, true);
});

test("knobValue: a disabled knob can never put a value on the wire", () => {
  const copilot = knobState(COPILOT, "copilot", "auto");
  // The human picked xhigh on claude, then switched the role to copilot. The
  // select is disabled now — but a stale DOM value must not survive into the
  // create_group payload.
  assert.equal(knobValue(copilot.effort, "xhigh"), "");
  const claude = knobState(CLAUDE, "claude", "sonnet");
  assert.equal(knobValue(claude.effort, "xhigh"), "xhigh");
  assert.equal(knobValue(claude.context, "1m"), "1m");
  // Same for the model gate: pick 1m on sonnet, switch the model to haiku.
  assert.equal(knobValue(knobState(CLAUDE, "claude", "haiku").context, "1m"), "");
  // And a value outside the CLI's own vocabulary never rides along either.
  assert.equal(knobValue(claude.effort, "banana"), "");
  assert.equal(knobValue(claude.effort, ""), "");
});

// ---------- opencode (#722): both knobs off, with the real seam named ----------

/** opencode's reply, verbatim from `CLI_CAPS` (slice A) — and *pinned*
 *  verbatim, not merely captioned so: the last test in this file reads both
 *  strings back out of the Rust source. Both value sets are empty and both notes
 *  are long, and that is the point: the effort note names a seam that genuinely
 *  EXISTS (`--variant` on `opencode run`, `agent.<name>.variant` in the
 *  generated config) and says why loomux does not write it yet. A shorter
 *  "unsupported" would have been a claim the source contradicts. */
const OPENCODE: CliKnobs = {
  cli: "opencode",
  known: true,
  effort: {
    values: [],
    note: "opencode's reasoning effort is a model VARIANT: a session flag on `opencode run` (--variant) but absent from the TUI loomux spawns, and settable per-agent in loomux's generated config (agent.<name>.variant, observed values minimal|high|max) — the seam exists, but the per-model vocabulary is provider-specific and unverified against a live run, so loomux does not write it yet",
  },
  context: {
    values: [],
    note: "opencode's context window is model-determined; no session-scoped variant switch is documented or present in the TUI's options",
  },
};

test("opencode renders both knobs disabled carrying opencode's own reason (#722)", () => {
  // No special case: an empty value set plus a note is already the honest shape,
  // and the generic path is what makes a fourth adapter cost the UI nothing. What
  // is pinned here is that opencode's caps flow through it UNCHANGED — a knob
  // silently enabled on an empty vocabulary would offer levels loomux cannot
  // deliver, and one hidden instead of disabled would read as loomux forgetting.
  const s = knobState(OPENCODE, "opencode", "opencode/deepseek-v4-flash-free");
  assert.equal(s.effort.enabled, false);
  assert.deepEqual(s.effort.values, []);
  assert.equal(s.effort.reason, OPENCODE.effort.note);
  assert.match(s.effort.reason, /--variant/, "the reason must name the seam that exists");
  assert.match(s.effort.reason, /agent\.<name>\.variant/);
  assert.equal(s.context.enabled, false);
  assert.deepEqual(s.context.values, []);
  assert.equal(s.context.reason, OPENCODE.context.note);
  assert.match(s.context.reason, /model-determined/);
});

test("no opencode model can turn a knob back on, `/` and all (#722)", () => {
  // `contextModelState`'s fail-open rule is about a claude id the gate does not
  // recognize; it must never reach an opencode row at all, because opencode's
  // own capability record says there is no context knob to gate. A
  // provider-prefixed id is exactly the shape that would slip past a
  // family-matching heuristic.
  for (const model of [
    "opencode/deepseek-v4-flash-free",
    "opencode/gpt-5.1-codex",
    "anthropic/claude-sonnet-4.6", // reads like a claude family id, but isn't claude
    "sonnet", // and neither is a claude alias typed onto an opencode row
    "",
  ]) {
    const s = knobState(OPENCODE, "opencode", model);
    assert.equal(s.context.enabled, false, `context must stay off for "${model}"`);
    assert.equal(s.effort.enabled, false, `effort must stay off for "${model}"`);
    // …and nothing may reach the wire through a disabled knob.
    assert.equal(knobValue(s.effort, "high"), "");
    assert.equal(knobValue(s.context, "1m"), "");
  }
});

test("a claude reply that arrives late must not enable knobs on an opencode row (#722)", () => {
  // The launcher memoizes one lookup per CLI and the human can move the picker
  // mid-flight. A mismatched reply is "not known", never "claude's answer".
  const stale = knobState(CLAUDE, "opencode", "opencode/deepseek-v4-flash-free");
  assert.equal(stale.effort.enabled, false);
  assert.equal(stale.context.enabled, false);
  assert.match(stale.effort.reason, /has not read opencode's capabilities yet/);
});

test("rev-237 finding 1: the opencode fixture's notes mirror the Rust source they're copied from (#722)", () => {
  // The fixture above is a hand-copied literal, so every assertion in this file
  // that compares against it is really the copy against itself: emptying or
  // REWORDING opencode's notes in `CLI_CAPS` cannot redden a frontend test. The
  // emptying half is already policed backend-side
  // (`src-tauri/tests/orchestration.rs`, which asserts every row's notes are
  // non-empty); DRIFT was policed nowhere, and a fixture that says "verbatim" and
  // isn't leaves this file asserting text the UI no longer renders, green.
  //
  // Same fix, same reasoning as `roster.test.ts`'s `MAX_AGENTS_CEILING` pin:
  // nothing at the type level ties a TS copy to a Rust literal, so read the Rust
  // back and fail loudly the day someone changes one without the other, rather
  // than trusting a caption to catch it.
  // Both production Rust source roots, not one hardcoded path (#888 slice A2
  // batch 4). `CLI_CAPS` moved from `src-tauri/src/orchestration/mod.rs` into
  // `crates/loomux-engine/src/model.rs`, and a reader pinned to the old file
  // does not fail with "the table moved" — `match` returns null and the assert
  // below reads as "the row's SHAPE changed", which is a different and wrong
  // diagnosis. The engine extraction has further batches to go, so scan every
  // root the table could live in and assert it is findable in exactly one:
  // that survives the next relocation, and it also catches the table being
  // duplicated rather than moved.
  const here = dirname(fileURLToPath(import.meta.url));
  const ROOTS = [
    join(here, "..", "src-tauri", "src"),
    join(here, "..", "crates", "loomux-engine", "src"),
  ];
  const rsFiles = (dir: string): string[] =>
    readdirSync(dir).flatMap((name) => {
      const p = join(dir, name);
      return statSync(p).isDirectory() ? rsFiles(p) : p.endsWith(".rs") ? [p] : [];
    });
  const ROW = /CliCaps \{\s*\n\s*cli: "opencode",[\s\S]*?\n {4}\},/;
  const hits = ROOTS.flatMap((root) => {
    const files = rsFiles(root);
    // A mistyped or stale root reads as zero files and would otherwise hide
    // behind the other root's hit — same per-root non-empty check
    // `src-tauri/tests/groupid.rs` makes for the same reason.
    assert.ok(files.length > 0, `no .rs files under ${root} — did the tree move?`);
    return files
      .map((f) => ({ file: f, m: readFileSync(f, "utf8").match(ROW) }))
      .filter((h) => h.m);
  });
  assert.equal(
    hits.length,
    1,
    `the opencode CLI_CAPS row must be findable in exactly one production source file, found ${hits.length} (${hits
      .map((h) => h.file)
      .join(", ")}) — either the row's shape changed and this pattern needs updating, or the table was duplicated instead of moved`
  );
  const row = hits[0]!.m!;
  // Report the file the scan actually matched, never a hardcoded name: the
  // whole point of the two-root scan above is that this row's home moves, and
  // an assertion message naming a fixed path is the same misdirection one
  // relocation later.
  const rowFile = hits[0]!.file;
  const declared = (key: string): string => {
    const m = row![0].match(new RegExp(`${key}: "([^"]*)"`));
    assert.ok(m, `opencode's ${key} must be a plain string literal in ${rowFile} — update this reader if it stops being one`);
    return m![1]!;
  };
  assert.equal(OPENCODE.effort.note, declared("effort_note"), "the effort note has drifted from CLI_CAPS");
  assert.equal(OPENCODE.context.note, declared("context_note"), "the context note has drifted from CLI_CAPS");
  // The value sets are the other half of the claim, and the one that decides
  // whether the knobs render at all: an opencode row that grew a vocabulary
  // backend-side must not keep being tested as though it had none.
  assert.match(row[0], /effort_levels: &\[\],/, "opencode's effort vocabulary is no longer empty in CLI_CAPS");
  assert.match(row[0], /context_variants: &\[\],/, "opencode's context vocabulary is no longer empty in CLI_CAPS");

  // Two OTHER test files match substrings of these notes — this file's
  // `--variant` / `agent.<name>.variant` assertions and
  // `workflowmodel.test.ts`'s finding-message ones. Assert them against the RUST
  // text, so a reword that kept those tests green by matching a stale copy fails
  // here instead.
  for (const needle of ["--variant", "agent.<name>.variant"]) {
    assert.ok(
      declared("effort_note").includes(needle),
      `tests match on "${needle}" — it must still be in CLI_CAPS' own effort note`
    );
  }
  assert.ok(declared("context_note").includes("model-determined"), "tests match on 'model-determined'");
});

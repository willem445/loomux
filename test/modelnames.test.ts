// Human-readable model names (#687 slice B). What these defend is not "the string
// is pretty" but two things a launcher picker can get wrong in ways that cost money:
//
//   1. The RAW ID IS NEVER LOST. It is what `--model` receives, what the vendor docs
//      are written in, and what a human retypes into `/model`. A label that replaced
//      it with a friendly name would make the dropdown unreadable against the docs.
//   2. NO INVENTED VENDOR CLAIMS. A description exists only where the vendor's own
//      reference states one (Claude Code's model-config alias table). Everything else
//      — copilot's `auto`, gemini's `pro`, a Bedrock ARN — gets the generic
//      prettifier, which reformats and claims nothing.

import { test } from "node:test";
import assert from "node:assert/strict";
import { modelLabel, modelOptionLabel, prettyModelId, INHERIT_MODEL_LABEL } from "../src/modelnames.ts";

test("a documented Claude Code alias carries the vendor's own description", () => {
  assert.equal(modelLabel("claude", "sonnet"), "sonnet — latest Sonnet, for daily coding tasks");
  assert.equal(modelLabel("claude", "opus"), "opus — latest Opus, for complex reasoning");
  assert.equal(modelLabel("claude", "haiku"), "haiku — fast, efficient Haiku for simple tasks");
  assert.equal(
    modelLabel("claude", "opusplan"),
    "opusplan — opus while planning, then sonnet to execute"
  );
});

test("the id is always the first thing in the label, verbatim", () => {
  // The label is read against the docs and retyped into `/model`; a description
  // that replaced the id would break both. Case is preserved too — the id is
  // echoed, not normalized, even though the LOOKUP is case-insensitive.
  assert.equal(
    modelLabel("claude", "Sonnet"),
    "Sonnet — latest Sonnet, for daily coding tasks"
  );
  for (const id of ["sonnet", "claude-opus-4-8", "gpt-5.3-codex", "auto"]) {
    assert.ok(modelLabel("copilot", id).startsWith(id), `label for ${id} must start with the id`);
  }
});

test("an alias description belongs to the CLI that documents it, not to every CLI", () => {
  // `sonnet` is a Claude Code alias. Copilot's docs say nothing about it, so
  // claiming "latest Sonnet, for daily coding tasks" on a copilot row would be
  // loomux inventing a vendor fact.
  assert.equal(modelLabel("copilot", "sonnet"), "sonnet");
  assert.equal(modelLabel("gemini", "opus"), "opus");
});

test("a versioned model id is prettified, never described", () => {
  assert.equal(modelLabel("copilot", "claude-sonnet-4.6"), "claude-sonnet-4.6 — Claude Sonnet 4.6");
  assert.equal(modelLabel("copilot", "gpt-5.3-codex"), "gpt-5.3-codex — GPT-5.3 Codex");
  assert.equal(modelLabel("claude", "claude-opus-4-8"), "claude-opus-4-8 — Claude Opus 4.8");
});

test("a prettified name that only re-cases the id adds nothing, so it is dropped", () => {
  // "auto — Auto" is noise in a dropdown. The suffix has to earn its width.
  assert.equal(modelLabel("copilot", "auto"), "auto");
  assert.equal(modelLabel("gemini", "pro"), "pro");
  assert.equal(modelLabel("claude", ""), "");
});

test("an id the prettifier cannot improve is passed through untouched", () => {
  // A Bedrock inference-profile ARN / provider-form id: reformatting it would
  // make it WRONG (it is a real identifier, not a name), so it stays verbatim.
  const arn = "us.anthropic.claude-sonnet-4-6-v1:0";
  assert.equal(modelLabel("claude", arn), arn);
});

test("the prettifier: hyphens become spaces, acronyms stay upper, versions rejoin", () => {
  assert.equal(prettyModelId("claude-sonnet-4.6"), "Claude Sonnet 4.6");
  assert.equal(prettyModelId("claude-opus-4-8"), "Claude Opus 4.8");
  assert.equal(prettyModelId("claude-haiku-4-5"), "Claude Haiku 4.5");
  assert.equal(prettyModelId("gpt-5.2"), "GPT-5.2");
  assert.equal(prettyModelId("gpt-5.3-codex"), "GPT-5.3 Codex");
  assert.equal(prettyModelId("gemini-2.5-pro"), "Gemini 2.5 Pro");
});

// ---------- provider-prefixed ids: opencode's `provider_id/model_id` (#722) ----------

test("a provider-prefixed id is named by its model half, and keeps the `/` in the id (#722)", () => {
  // The one thing that must never happen to an opencode id is the mangle slice A
  // fixed in the backend: dropping the `/` turns
  // `opencode/deepseek-v4-flash-free` into a model that does not exist. The label
  // shows the id verbatim and adds the vendor's own name for it.
  assert.equal(
    modelLabel("opencode", "opencode/deepseek-v4-flash-free"),
    "opencode/deepseek-v4-flash-free — DeepSeek V4 Flash Free"
  );
  assert.equal(modelLabel("opencode", "opencode/gpt-5.1-codex"), "opencode/gpt-5.1-codex — GPT-5.1 Codex");
  // The NAME drops the provider (the id in front already carries it — repeating
  // it is width spent on nothing); the ID never loses a character of it.
  assert.equal(prettyModelId("opencode/deepseek-v4-flash-free"), "DeepSeek V4 Flash Free");
  for (const id of [
    "opencode/deepseek-v4-flash-free",
    "opencode/deepseek-v4-flash",
    "opencode/gpt-5.1-codex",
    "anthropic/claude-sonnet-4.6",
  ]) {
    assert.ok(modelLabel("opencode", id).startsWith(`${id} `), `the id must survive verbatim: ${id}`);
    assert.ok(modelLabel("opencode", id).includes("/"), `the provider separator must survive: ${id}`);
  }
});

test("`v4` is a word, and DeepSeek is spelled the way its vendor spells it (#722)", () => {
  // "DeepSeek V4 Flash Free" is the Zen catalog's own name for it. Title-casing
  // would give "Deepseek", and binding the version would give "DeepSeek.V4" —
  // both are loomux mis-spelling somebody else's product.
  assert.equal(prettyModelId("deepseek-v4-flash-free"), "DeepSeek V4 Flash Free");
  assert.equal(prettyModelId("deepseek-v4-flash"), "DeepSeek V4 Flash");
});

test("an id that merely CONTAINS a `/` is not split into a provider and a model", () => {
  // A Bedrock inference-profile ARN has slashes and is not a `provider/model`
  // id. Half-rewriting one would produce a name that is wrong rather than
  // pretty, which is the outcome the prettifier exists to avoid.
  const arn =
    "arn:aws:bedrock:us-east-1:123456789012:application-inference-profile/us.anthropic.claude-sonnet-4-6-v1:0";
  assert.equal(prettyModelId(arn), arn);
  assert.equal(modelLabel("claude", arn), arn);
  // Two slashes is not the two-part form either, and a `/` with nothing after it
  // names no model at all.
  assert.equal(prettyModelId("a/b/claude-sonnet-4.6"), "a/b/claude-sonnet-4.6");
  assert.equal(prettyModelId("opencode/"), "opencode/");
  // A provider whose model half has no name to give back stays an identifier in
  // full — never a bare prefix with the model stripped off it.
  assert.equal(prettyModelId("opencode/auto"), "opencode/auto");
  assert.equal(modelLabel("opencode", "opencode/auto"), "opencode/auto");
});

// ---------- the "no --model at all" row (#722) ----------

test("the empty curated id renders as a real row, not a blank one (#722)", () => {
  // opencode is the one CLI with no alias to default to, so its list offers
  // "inherit whatever the human configured" as an actual option. An option
  // rendered from `modelLabel("")` would be a blank line to guess at.
  assert.equal(modelOptionLabel("opencode", ""), INHERIT_MODEL_LABEL);
  assert.ok(INHERIT_MODEL_LABEL.trim().length > 0, "the inherit row must have text");
  // …and it says nothing about which model that is, because loomux does not know.
  assert.ok(
    !/deepseek|sonnet|opus|gpt/i.test(INHERIT_MODEL_LABEL),
    "the inherit row must not name a model loomux is not selecting"
  );
  // Whitespace is not a model id either.
  assert.equal(modelOptionLabel("opencode", "   "), INHERIT_MODEL_LABEL);
  // Every non-empty id goes through `modelLabel` unchanged — one label rule, not two.
  for (const [cli, id] of [
    ["claude", "sonnet"],
    ["opencode", "opencode/deepseek-v4-flash-free"],
    ["copilot", "auto"],
  ] as const) {
    assert.equal(modelOptionLabel(cli, id), modelLabel(cli, id));
  }
  // `modelLabel` itself keeps its own contract: an empty id has no NAME.
  assert.equal(modelLabel("opencode", ""), "");
});

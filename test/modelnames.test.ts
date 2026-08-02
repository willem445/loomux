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
import { modelLabel, prettyModelId } from "../src/modelnames.ts";

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

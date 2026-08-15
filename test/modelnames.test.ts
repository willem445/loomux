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
import {
  INHERIT_MODEL_LABEL,
  detectedModelOptionLabel,
  modelLabel,
  modelOptionLabel,
  modelSummaryLine,
  prettyModelId,
} from "../src/modelnames.ts";
// The table itself, so the composed-path tests can assert the layer below them
// agrees — a pin that only checked the composed answer could not tell "the
// table refused" from "the table has a row I forgot about".
import { contextWindowFor } from "../src/modelcontext.ts";
import type { ModelDetail } from "../src/modelcatalog.ts";

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

// ---- labels that prefer what the CLI itself reported (#993) ----------------

const detail = (over: Partial<ModelDetail> & { id: string }): ModelDetail => ({
  resolvedId: "",
  name: "",
  description: "",
  supportsEffort: null,
  effortLevels: [],
  ...over,
});

test("with nothing detected a row reads exactly as it always did", () => {
  // Every caller that predates detection passes `null`, so this is the whole
  // no-regression surface for the label path.
  for (const id of ["sonnet", "claude-sonnet-4.6", "auto", ""]) {
    assert.equal(detectedModelOptionLabel("claude", id, null), modelOptionLabel("claude", id));
  }
});

test("a name the human's own install printed outranks the table and the prettifier", () => {
  // `sonnet` has a quoted alias description and `claude-opus-4-8` prettifies
  // cleanly — a reported name has to beat both, because it is what their CLI's
  // own /model picker shows them.
  assert.equal(
    detectedModelOptionLabel("claude", "sonnet", detail({ id: "sonnet", name: "Sonnet 5" })),
    "sonnet — Sonnet 5"
  );
  assert.equal(
    detectedModelOptionLabel("claude", "opus[1m]", detail({ id: "opus[1m]", name: "Opus (1M context)" })),
    "opus[1m] — Opus (1M context)"
  );
});

test("the raw id is never lost, and a name that only re-cases it earns no space", () => {
  const label = detectedModelOptionLabel("copilot", "gpt-5.2", detail({ id: "gpt-5.2", name: "GPT-5.2" }));
  assert.ok(label.startsWith("gpt-5.2"), `the id leads: ${label}`);
  assert.equal(
    detectedModelOptionLabel("copilot", "auto", detail({ id: "auto", name: "Auto" })),
    modelOptionLabel("copilot", "auto"),
    "`auto — Auto` is width spent on nothing (the #687 rule, unchanged)"
  );
});

test("the inherit row keeps its own wording whatever a reply says", () => {
  assert.equal(detectedModelOptionLabel("claude", "", detail({ id: "", name: "Default" })), INHERIT_MODEL_LABEL);
});

test("the summary line states only what has a source", () => {
  assert.equal(modelSummaryLine("claude", "sonnet", null), "1M context", "the table alone still has something to say");
  assert.equal(modelSummaryLine("copilot", "gpt-5.2", null), "", "nothing known means nothing shown, not an empty frame");
  assert.equal(
    modelSummaryLine(
      "claude",
      "opus",
      detail({ id: "opus", description: "Best for everyday, complex tasks", supportsEffort: true, effortLevels: ["low", "max"] })
    ),
    "Best for everyday, complex tasks · effort: low, max · 1M context"
  );
});

test("the window is looked up against the id the CLI resolved the alias to", () => {
  // `sonnet` happens to have its own row, so the case that proves the
  // precedence is one where only the RESOLVED id does — an alias this build has
  // no row for, resolving to a model it does.
  assert.equal(modelSummaryLine("claude", "sonnet", null), "1M context");
  assert.equal(
    modelSummaryLine("claude", "latest", detail({ id: "latest", resolvedId: "claude-haiku-4-5" })),
    "200K context",
    "the canonical id the install resolved to is the one a static table can be sure about"
  );
});

test("a resolved model with no table row says nothing — it never inherits its family's window", () => {
  // #997 review, blocking 2. The first cut branched on the LABEL being empty
  // instead of the FIELD being absent, so a reported `resolvedModel` the table
  // has no row for fell through to the alias and printed the alias's number.
  //
  // `claude-sonnet-4-5` is the exact case `modelcontext.ts` rule 3 names and
  // `test/modelcontext.test.ts` already pins one layer down — this is the
  // composed path re-opening the hole the table was built to close, and it only
  // opened once detection was on.
  assert.equal(contextWindowFor("claude", "claude-sonnet-4-5"), null, "the table itself refuses to answer…");
  assert.equal(
    modelSummaryLine("claude", "sonnet", detail({ id: "sonnet", resolvedId: "claude-sonnet-4-5" })),
    "",
    "…so the line composed from it must refuse too, rather than borrowing `sonnet`'s 1M"
  );
});

test("a resolved id an enterprise install really produces does not inherit one either", () => {
  // The same defect on the id shapes a Bedrock or gateway deployment reports —
  // a fresh fixture rather than a re-run of the alias case, because these are
  // what an install that never sees a bare `claude-*` id actually resolves to.
  for (const resolvedId of [
    "us.anthropic.claude-sonnet-4-5-v1:0",
    "arn:aws:bedrock:us-east-1:1:inference-profile/anthropic.claude-opus",
    "my-gateway-deployment",
  ]) {
    assert.equal(
      modelSummaryLine("claude", "sonnet", detail({ id: "sonnet", resolvedId })),
      "",
      `${resolvedId} has no row, so nothing may be claimed for it`
    );
  }
});

test("an absent resolvedModel is a different state from an unknown one", () => {
  // The fallback is for the field being MISSING — an install older than Claude
  // Code v2.1.197, which simply omits it. There the picked id is the most
  // specific thing loomux has, so it is what gets looked up. This is the half
  // the fix must keep, and the half the original test pinned correctly.
  assert.equal(
    modelSummaryLine("claude", "haiku", detail({ id: "haiku", resolvedId: "" })),
    "200K context",
    "absent means `ask the id instead`"
  );
  assert.equal(
    modelSummaryLine("claude", "haiku", detail({ id: "haiku", resolvedId: "   " })),
    "200K context",
    "and whitespace is absent, not a model name"
  );
  assert.equal(
    modelSummaryLine("claude", "haiku", detail({ id: "haiku", resolvedId: "claude-haiku-4-5" })),
    "200K context",
    "a resolved id the table DOES place answers from that row"
  );
});

test("a context window is never scraped out of the reported prose", () => {
  // The description really does read "Opus 5 with 1M context". Parsing a number
  // out of a sentence is the manufacture `modelwire.ts` forbids, so on a CLI
  // whose windows loomux does not document, the prose is shown and no number is
  // claimed.
  const line = modelSummaryLine("copilot", "some-model", detail({ id: "some-model", description: "Runs with 1M context" }));
  assert.equal(line, "Runs with 1M context");
  assert.ok(!line.includes("1M context ·") && !line.endsWith("· 1M context"), `no window was invented: ${line}`);
});

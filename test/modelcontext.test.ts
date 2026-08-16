import { test } from "node:test";
import assert from "node:assert/strict";

import { contextWindowFor, contextWindowLabel, formatTokens } from "../src/modelcontext.ts";

// #993. The table is the one model fact loomux states on its own authority
// (every other one is read out of a CLI's own reply), so these tests are mostly
// about the three ways such a table goes wrong: it answers for a vendor that
// documents nothing, it answers for a version that does not share its family's
// number, and it answers with a rounded figure that is not the real one.

test("a family alias carries the window its vendor documents", () => {
  assert.equal(contextWindowFor("claude", "sonnet"), 1_000_000);
  assert.equal(contextWindowFor("claude", "opus"), 1_000_000);
  assert.equal(contextWindowFor("claude", "fable"), 1_000_000);
  assert.equal(
    contextWindowFor("claude", "haiku"),
    200_000,
    "haiku is the row that proves the table is read rather than defaulted — every other claude family is 1M, " +
      "so a lookup that always returned 1M would pass every other assertion here"
  );
});

test("a window is a vendor's fact, so it is not lent to another CLI", () => {
  // #687's rule, applied to a number instead of a description: `sonnet` is a
  // real copilot model id, but GitHub's reference says neither which Sonnet it
  // serves nor what window it serves it with. Answering 1M there would be
  // loomux putting a figure in a vendor's mouth — and it would be shown beside
  // a model that may not have it.
  assert.equal(contextWindowFor("copilot", "sonnet"), null);
  assert.equal(contextWindowFor("copilot", "claude-sonnet-4.6"), null);
  assert.equal(contextWindowFor("gemini", "pro"), null);
  assert.equal(contextWindowFor("opencode", "opencode/gpt-5.1-codex"), null);
});

test("a versioned id gets a row only where the vendor documents that version", () => {
  // The failure this exists to prevent: resolving `claude-sonnet-4-5` to the
  // `sonnet` FAMILY and reporting the 1M window the alias resolves to today.
  // It is a Sonnet; its window is not that one. Nothing is the right answer.
  assert.equal(contextWindowFor("claude", "claude-sonnet-4-6"), 1_000_000);
  assert.equal(
    contextWindowFor("claude", "claude-sonnet-4-5"),
    null,
    "an undocumented version must not inherit its family's number"
  );
  assert.equal(contextWindowFor("claude", "claude-haiku-4-5"), 200_000);
});

test("an account-resolved alias has no model to look a window up for", () => {
  // `best` and `default` resolve per account (the same reason `contextModelState`
  // refuses them `[1m]`), so there is no single model whose window this could be.
  assert.equal(contextWindowFor("claude", "best"), null);
  assert.equal(contextWindowFor("claude", "default"), null);
});

test("the [1m] suffix states the window itself, whatever the base id says", () => {
  // The suffix's documented meaning is "a 1 million token context window", so it
  // is the more specific statement and outranks the table. The haiku case is the
  // one that matters: its base row is 200K, and a pane launched with `haiku[1m]`
  // is not running a 200K window.
  assert.equal(contextWindowFor("claude", "sonnet[1m]"), 1_000_000);
  assert.equal(contextWindowFor("claude", "haiku[1m]"), 1_000_000);
  assert.equal(contextWindowFor("claude", "claude-opus-4-8[1m]"), 1_000_000);
});

test("an id loomux cannot resolve to a model yields nothing, never a guess", () => {
  assert.equal(contextWindowFor("claude", ""), null);
  assert.equal(contextWindowFor("", "sonnet"), null);
  assert.equal(contextWindowFor("claude", "claude-sonnet-9-9"), null, "a model newer than this build");
  assert.equal(
    contextWindowFor("claude", "arn:aws:bedrock:us-east-1:1:inference-profile/anthropic.claude-opus"),
    null,
    "an ARN is an identifier, not a model name — it must not be split into one"
  );
});

test("an opencode-style id is looked up by its model half", () => {
  // The provider is routing, not identity: the same model behind
  // `anthropic/` is the same model. Only the narrow two-part form splits.
  assert.equal(contextWindowFor("claude", "anthropic/claude-opus-4-8"), 1_000_000);
  assert.equal(contextWindowFor("claude", "openrouter/anthropic/claude-opus-4-8"), null, "three parts is not the two-part form");
});

test("ids are matched case- and whitespace-insensitively", () => {
  assert.equal(contextWindowFor("claude", "  Sonnet  "), 1_000_000);
  assert.equal(contextWindowFor("CLAUDE", "Claude-Haiku-4-5"), 200_000);
  assert.equal(contextWindowFor("claude", "SONNET[1M]"), 1_000_000);
});

test("a window is abbreviated only where the abbreviation is exact", () => {
  assert.equal(formatTokens(1_000_000), "1M");
  assert.equal(formatTokens(200_000), "200K");
  assert.equal(formatTokens(128_000), "128K");
  assert.equal(
    formatTokens(1_048_576),
    "1048576",
    "a window that is not a round million must not be shown as one"
  );
});

test("the label says nothing when there is nothing documented", () => {
  assert.equal(contextWindowLabel("claude", "sonnet"), "1M context");
  assert.equal(contextWindowLabel("claude", "haiku"), "200K context");
  assert.equal(contextWindowLabel("copilot", "sonnet"), "", "silence, not `0 context`");
  assert.equal(contextWindowLabel("claude", "claude-sonnet-9-9"), "");
});

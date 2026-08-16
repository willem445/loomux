import { test } from "node:test";
import assert from "node:assert/strict";

import { parseListModelsReply } from "../src/modelwire.ts";

// #993. Every fixture here is WRITTEN, not captured: constraint 3 forbids
// running a real agent CLI to collect one, so these are composed from the shape
// Anthropic publishes for `ModelInfo` and the control-response envelope
// (`sdk.d.ts`; docs.claude.com/en/api/agent-sdk/typescript). The human does the
// live validation. What the fixtures have to be is *representative on the
// points that can go wrong* — an optional capability field that is genuinely
// absent, a reply that arrives after unrelated stream lines, and a payload
// whose shape loomux does not recognise.

/** The correlation id the backend puts on its request and hands back with the
 *  reply. A literal here rather than an import, so a drift between the two
 *  sides would show up as a failing test instead of a silent non-match. */
const REQ = "loomux-list-models";

/** One line of the `--output-format stream-json` transcript. */
const line = (o: unknown): string => JSON.stringify(o);

const reply = (models: unknown[], requestId = REQ): string =>
  line({ type: "control_response", response: { subtype: "success", request_id: requestId, response: { models } } });

/** Shaped like the rows Claude Code reports. `opus[1m]` is a row the CLI LISTS —
 *  loomux does not compose it — and `haiku` deliberately carries no effort
 *  fields at all, because every capability field on `ModelInfo` is optional and
 *  a parser that treats absent as `false` would be wrong about this exact row. */
const OPUS_ROW = {
  value: "opus[1m]",
  resolvedModel: "claude-opus-5[1m]",
  displayName: "Opus (1M context)",
  description: "Opus 5 with 1M context · Best for everyday, complex tasks",
  supportsEffort: true,
  supportedEffortLevels: ["low", "medium", "high", "xhigh", "max"],
  supportsFastMode: true,
};
const HAIKU_ROW = {
  value: "haiku",
  resolvedModel: "claude-haiku-4-5",
  displayName: "Haiku",
  description: "Fast and efficient for simple tasks",
};

/** A transcript with the reply buried in the session's other stream-json lines,
 *  because the reply's position relative to them is undocumented — a parser
 *  that assumed a position would fail silently on a build that moved it. */
const FULL_TRANSCRIPT = [
  `{"type":"system","subtype":"init","session_id":"abc","tools":[]}`,
  "",
  "Some banner text that is not JSON at all",
  reply([OPUS_ROW, HAIKU_ROW]),
  `{"type":"result","subtype":"success","is_error":false}`,
].join("\n");

test("the reply's own rows become the catalog, in the CLI's order", () => {
  const report = parseListModelsReply(FULL_TRANSCRIPT, REQ);
  assert.equal(report.error, null);
  assert.deepEqual(
    report.models.map((m) => m.id),
    ["opus[1m]", "haiku"],
    "ids are the verbatim `value` strings, in the order the CLI listed them"
  );
  assert.equal(report.models[0].resolvedId, "claude-opus-5[1m]");
  assert.equal(report.models[0].name, "Opus (1M context)");
  assert.deepEqual(report.models[0].effortLevels, ["low", "medium", "high", "xhigh", "max"]);
});

test("a row that reports no effort fields is unknown, not unsupported", () => {
  // The whole reason `supportsEffort` is three-state. `haiku` really does come
  // back without the fields; reading that as `false` would turn the effort knob
  // OFF for a model whose CLI never said anything about it.
  const haiku = parseListModelsReply(FULL_TRANSCRIPT, REQ).models[1];
  assert.equal(haiku.supportsEffort, null, "absent must not collapse into false");
  assert.deepEqual(haiku.effortLevels, []);
});

test("levels are ignored unless the row says effort is supported", () => {
  // `supportsEffort` and `supportedEffortLevels` are separate optionals. A row
  // that says effort is NOT supported must not hand the knob values to offer,
  // however many levels sit beside the flag.
  const report = parseListModelsReply(
    reply([{ value: "auto", displayName: "Auto", description: "", supportsEffort: false, supportedEffortLevels: ["low", "high"] }]),
    REQ
  );
  assert.equal(report.models[0].supportsEffort, false);
  assert.deepEqual(report.models[0].effortLevels, [], "a stated no outranks the levels beside it");
});

test("a row with no usable id names no model and is dropped", () => {
  // The one manufacture this parser could commit: inventing an id from a
  // display name, or emitting a blank one that renders as the inherit row.
  const report = parseListModelsReply(
    reply([{ displayName: "Sonnet", description: "no value field" }, { value: "   " }, "sonnet", 42, null, OPUS_ROW]),
    REQ
  );
  assert.deepEqual(report.models.map((m) => m.id), ["opus[1m]"], "only the row that named itself survives");
});

test("a duplicated id keeps the CLI's first listing", () => {
  const report = parseListModelsReply(reply([OPUS_ROW, HAIKU_ROW, { ...OPUS_ROW, displayName: "Later duplicate" }]), REQ);
  assert.deepEqual(report.models.map((m) => m.id), ["opus[1m]", "haiku"]);
  assert.equal(report.models[0].name, "Opus (1M context)", "the first row wins; the menu is not re-ordered");
});

test("a CLI too old for the request reports why, and offers nothing", () => {
  // The documented answer from a build that predates the subtype. It is a fact
  // about the install, not a fault: no models, and a reason a human can read.
  const old = line({
    type: "control_response",
    response: { subtype: "error", request_id: REQ, error: "Unsupported control request subtype: list_models" },
  });
  const report = parseListModelsReply(old, REQ);
  assert.deepEqual(report.models, []);
  assert.match(report.error ?? "", /Unsupported control request subtype/);
});

test("a payload loomux does not recognise yields nothing rather than a guess", () => {
  // `ControlResponse.response` is typed `Record<string, unknown>` and the
  // `models` key is UNVERIFIED, so this is the case that decides whether the
  // feature degrades safely. It must look exactly like "no reply".
  const odd = line({ type: "control_response", response: { subtype: "success", request_id: REQ, response: { catalog: [OPUS_ROW] } } });
  assert.deepEqual(parseListModelsReply(odd, REQ), { models: [], error: null });
  assert.deepEqual(parseListModelsReply("", REQ), { models: [], error: null });
  assert.deepEqual(parseListModelsReply("not json at all\n{broken\n", REQ), { models: [], error: null });
});

test("a reply to somebody else's control request is not read as this one's", () => {
  // The correlation id earns its place here: a permission prompt or any other
  // control round-trip on the same stream also answers with a
  // `control_response`, and reading one as the model list would publish
  // whatever it happened to carry.
  const other = reply([OPUS_ROW], "some-other-request");
  assert.deepEqual(parseListModelsReply(other, REQ).models, [], "a mismatched request_id is not our answer");
  assert.deepEqual(
    parseListModelsReply(other, "").models.map((m) => m.id),
    ["opus[1m]"],
    "with no id to correlate on, any successful reply is better than none"
  );
});

test("CRLF line endings parse the same as LF", () => {
  // Windows is the baseline platform; a parser that split on LF alone would
  // leave a trailing CR inside the JSON and fail on every line.
  const report = parseListModelsReply(FULL_TRANSCRIPT.split("\n").join("\r\n"), REQ);
  assert.deepEqual(report.models.map((m) => m.id), ["opus[1m]", "haiku"]);
});

test("a truncated line is skipped, never repaired", () => {
  const truncated = `{"type":"control_response","response":{"subtype":"success","request_id":"${REQ}","response":{"models":[{"value":"opu`;
  assert.deepEqual(parseListModelsReply(truncated, REQ), { models: [], error: null });
});

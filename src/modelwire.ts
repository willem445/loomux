// Reading a CLI's own list-models reply off its stdout (#993).
//
// DOM-free and I/O-free, so `test/modelwire.test.ts` can pin every rule below
// against fixture bytes. **No test in this repo may run a real agent CLI**
// (constraint 3 — it spends the human's money), so the fixtures are written
// from the vendor's published type, not captured from a run, and the human does
// the live validation. The backend that actually spawns the CLI
// (`src-tauri/src/modelwire.rs`) deliberately parses none of this: it moves
// bytes, and the shape lives here where a `node --test` round can exercise it.
//
// **What the vendor documents, and what it does not.** Anthropic types the
// envelope in `@anthropic-ai/claude-agent-sdk`'s `sdk.d.ts` and publishes the
// model row at <https://docs.claude.com/en/api/agent-sdk/typescript> (read
// 2026-08-14, per the `agent-cli-reference` discipline):
//
//     export declare type SDKControlResponse = {
//         type: 'control_response';
//         response: ControlResponse | ControlErrorResponse;
//     };
//     declare type ControlResponse = {
//         subtype: 'success';
//         request_id: string;
//         response?: Record<string, unknown>;
//         ...
//     };
//     declare type ControlErrorResponse = {
//         subtype: 'error';
//         request_id: string;
//         error: string;
//         ...
//     };
//     export declare type ModelInfo = {
//         value: string;
//         resolvedModel?: string;
//         displayName: string;
//         description: string;
//         supportsEffort?: boolean;
//         supportedEffortLevels?: ('low' | 'medium' | 'high' | 'xhigh' | 'max')[];
//         ...
//     };
//
// The gap is deliberate and worth naming: `ControlResponse.response` is typed
// `Record<string, unknown>`, and the string `list_models` appears NOWHERE in
// Anthropic's documentation corpus. **That the payload's key is `models` is
// therefore UNVERIFIED** — it is attested by `stablyai/orca`'s probe (MIT) and
// its captured fixtures, and circumstantially by the sibling *initialize*
// response, which IS typed and does carry `models: ModelInfo[]`. Docs-are-
// silent, not docs-say-X, and this module is built for that: an unrecognised
// payload yields no models and every caller keeps its seed list, which is the
// same outcome as a CLI too old to know the request at all.
//
// So the one property this module guarantees is the one `cliprobe.rs` already
// guarantees for the help/enumerator parsers, and for the same reason: **it may
// under-recognise, but it must never manufacture.** Every id it returns is a
// verbatim `value` string from the CLI's own JSON. It does not repair a
// truncated line, does not infer an id from a display name, and does not read a
// context window out of the description prose — the description says "Opus 5
// with 1M context", and scraping a number out of a sentence is exactly the
// manufacture this rule forbids. The numeric window comes from
// `modelcontext.ts`, which cites a page.

import type { ModelDetail, ModelReport } from "./modelcatalog.ts";

/** A `Record`-shaped value, or `null` for anything else. Narrowing helper: the
 *  reply is untrusted JSON, so every hop is checked rather than asserted. */
function obj(v: unknown): Record<string, unknown> | null {
  return typeof v === "object" && v !== null && !Array.isArray(v) ? (v as Record<string, unknown>) : null;
}

/** A trimmed string, or `""` for a value that was not a string. `""` is this
 *  module's "the CLI did not say" for every text field — never a placeholder
 *  that a surface could mistake for content. */
function str(v: unknown): string {
  return typeof v === "string" ? v.trim() : "";
}

/** One `ModelInfo` row, or `null` when the entry is not one.
 *
 *  The gate is `value`: a row with no non-empty string `value` names no model,
 *  so there is nothing to offer and nothing to look a capability up against.
 *  Everything else is optional on the vendor's own type — a real `haiku` row
 *  comes back with no effort fields at all — so an absent field becomes `""` or
 *  `null`, never a fabricated default.
 *
 *  **`supportedEffortLevels` is honoured only when `supportsEffort` is exactly
 *  `true`.** The two fields are separate optionals on the vendor type, and the
 *  levels are the answer to a question the flag says was not asked otherwise:
 *  publishing levels for a model whose reply says it takes no effort setting
 *  would hand `selectorknobs.ts` values it must not offer. */
function parseModel(entry: unknown): ModelDetail | null {
  const row = obj(entry);
  if (!row) return null;
  const id = str(row.value);
  if (!id) return null;
  const supportsEffort = typeof row.supportsEffort === "boolean" ? row.supportsEffort : null;
  const effortLevels =
    supportsEffort === true && Array.isArray(row.supportedEffortLevels)
      ? row.supportedEffortLevels.filter((l): l is string => typeof l === "string" && l.trim() !== "").map((l) => l.trim())
      : [];
  return {
    id,
    resolvedId: str(row.resolvedModel),
    name: str(row.displayName),
    description: str(row.description),
    supportsEffort,
    effortLevels,
  };
}

/** Read a `list_models` reply out of a CLI's stdout.
 *
 *  `requestId` is the id the BACKEND put on the request it sent, handed back on
 *  the same reply rather than duplicated as a constant on both sides — a
 *  correlation id that drifted between sender and reader would silently stop
 *  correlating, and nothing would look wrong. It is what keeps a
 *  `control_response` to some *other* control request from being read as the
 *  answer to this one; `""` means "the backend did not say", and then any
 *  successful reply is accepted, because a wrong-reply risk beats no reply at
 *  all on a path that already degrades to the seed.
 *
 *  Every line is examined and the first usable one wins. Ordering is
 *  deliberately not modelled: the reply's position relative to the session's
 *  `system`/`init` lines is undocumented, so assuming one would be a guess with
 *  a silent failure mode. Non-JSON chatter, banners and CRLF endings all simply
 *  fail to parse and are skipped.
 *
 *  Never throws. A reply loomux cannot read is a reply loomux does without. */
export function parseListModelsReply(stdout: string, requestId = ""): ModelReport {
  let firstError: string | null = null;
  for (const raw of stdout.split(/\r?\n/)) {
    const line = raw.trim();
    if (!line.startsWith("{")) continue;
    let parsed: unknown;
    try {
      parsed = JSON.parse(line);
    } catch {
      continue;
    }
    const env = obj(parsed);
    if (!env || env.type !== "control_response") continue;
    const body = obj(env.response);
    if (!body) continue;
    // Correlate before reading anything out of the body.
    if (requestId && str(body.request_id) !== requestId) continue;
    if (body.subtype === "error") {
      // The documented answer from a CLI that predates the request — it is a
      // fact about the install, not a fault, so it is reported once and the
      // scan continues in case a usable reply follows.
      if (firstError === null) firstError = str(body.error) || "the CLI rejected the list-models request";
      continue;
    }
    if (body.subtype !== "success") continue;
    const payload = obj(body.response);
    // UNVERIFIED key — see the module note. Not an array means loomux does not
    // recognise this reply, which is the same outcome as no reply.
    if (!payload || !Array.isArray(payload.models)) continue;
    const models: ModelDetail[] = [];
    const seen = new Set<string>();
    for (const entry of payload.models) {
      const model = parseModel(entry);
      // First row wins a duplicated id: the reply is the CLI's own ordered menu,
      // and re-ordering it would put loomux's opinion in front of the vendor's.
      if (model && !seen.has(model.id)) {
        seen.add(model.id);
        models.push(model);
      }
    }
    if (models.length) return { models, error: null };
  }
  return { models: [], error: firstError };
}

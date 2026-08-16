// Human-readable model names for the launcher's model pickers (#687).
//
// DOM-free and I/O-free, so `test/modelnames.test.ts` can pin it directly.
//
// Two rules, and the second is the one that keeps this file honest:
//
//  1. **The raw id is never lost.** It is what `--model` receives, what the
//     vendor's docs are written in, and what a human retypes into `/model`. So a
//     label is `"<id> — <name>"`, never `"<name>"` — and the suffix is dropped
//     entirely when it would only re-case the id (`auto — Auto` is width spent
//     on nothing).
//  2. **A description exists only where a vendor documents one.** The map below
//     is Claude Code's own alias table, condensed (model-config §Model aliases,
//     fetched 2026-08-02 per the `agent-cli-reference` discipline). Copilot's
//     `auto`, gemini's `pro`, a Bedrock ARN: their references say nothing about
//     what those mean, so loomux says nothing either and they fall to the
//     prettifier, which reformats and CLAIMS nothing. The three-state rule —
//     docs-say-X / docs-are-silent / docs-say-NOT-X — applied to a dropdown.
//
// This is also why there is no id→name TABLE (the #329 lesson: model tables age
// badly, and a stale one is worse than none). Aliases change slowly and are
// documented by name; versioned ids are handled by a formatter that never needs
// to know a model exists.

import { contextWindowLabel } from "./modelcontext.ts";
import type { ModelDetail } from "./modelcatalog.ts";

/** Per-CLI alias descriptions, keyed by the CLI id the launcher/workflow uses.
 *
 *  Claude Code, from model-config §Model aliases:
 *    default  — "clears any model override and reverts to the recommended model
 *                for your account type"
 *    best     — "Uses Fable 5 where your organization has access to it,
 *                otherwise the latest Opus model"
 *    fable    — "Uses Claude Fable 5 for your hardest and longest-running tasks"
 *    sonnet   — "Uses the latest Sonnet model for daily coding tasks"
 *    opus     — "Uses the latest Opus model for complex reasoning tasks"
 *    haiku    — "Uses the fast and efficient Haiku model for simple tasks"
 *    opusplan — "uses `opus` during plan mode, then switches to `sonnet` for
 *                execution"
 *
 *  An alias is a property of the CLI that documents it, so the map is keyed by
 *  CLI: `sonnet` on a copilot row gets no description, because GitHub's CLI
 *  reference does not define one and inventing it would be loomux putting words
 *  in a vendor's mouth. */
const ALIAS_DESCRIPTIONS: Record<string, Record<string, string>> = {
  claude: {
    sonnet: "latest Sonnet, for daily coding tasks",
    opus: "latest Opus, for complex reasoning",
    haiku: "fast, efficient Haiku for simple tasks",
    fable: "Claude Fable 5, for the hardest and longest-running tasks",
    opusplan: "opus while planning, then sonnet to execute",
    best: "Fable 5 where your organization has it, else the latest Opus",
    default: "clears the model override, back to your account's recommended model",
  },
};

/** Words that are acronyms rather than names, and stay upper-cased. A version
 *  number binds to one with a hyphen (`GPT-5.3`) rather than a space, which is
 *  how the vendor writes it. */
const ACRONYMS = new Set(["gpt", "api", "cli"]);

/** Words whose own vendor spelling is neither lower-case nor title-case, keyed
 *  by the lower-cased token. A BRAND, never a model: brands are stable, which is
 *  what keeps this out of the #329 "model tables age badly" trap — the entry
 *  spells a name the vendor has already committed to, and says nothing about
 *  which of that vendor's models exist.
 *
 *  `deepseek` — the OpenCode Zen catalog lists it as "DeepSeek V4 Flash Free"
 *  (Zen docs, per the `agent-cli-reference` discipline), so title-casing it to
 *  "Deepseek" would be loomux mis-spelling somebody's product. */
const WORD_CASINGS: Record<string, string> = { deepseek: "DeepSeek" };

const isVersionToken = (t: string): boolean => /^\d+(\.\d+)*$/.test(t);
const isWordToken = (t: string): boolean => /^[A-Za-z]+$/.test(t);
/** `v4` in `deepseek-v4-flash-free`: a version written with its own `v`, which
 *  the vendor renders as a separate word ("DeepSeek V4 Flash Free") rather than
 *  binding it to the previous token the way a bare `4` binds. */
const isVeeVersionToken = (t: string): boolean => /^v\d+(\.\d+)*$/.test(t);

/** A provider id is the part before the `/` in opencode's `provider_id/model_id`
 *  ids. Recognized narrowly on purpose: lower-case identifier characters only,
 *  so a Bedrock ARN or any other identifier that merely happens to contain a `/`
 *  is NOT split and goes on falling through to the untouched-passthrough. */
const isProviderId = (t: string): boolean => /^[a-z0-9][a-z0-9._-]*$/.test(t);

/** `claude-sonnet-4.6` → `Claude Sonnet 4.6`, `gpt-5.3-codex` → `GPT-5.3 Codex`,
 *  `opencode/deepseek-v4-flash-free` → `DeepSeek V4 Flash Free`.
 *
 *  Structural only: it splits on `-`, title-cases words, upper-cases known
 *  acronyms, and rejoins version segments (`4-8` → `4.8`, the way Anthropic
 *  writes the model it names). It knows no model, so it cannot go stale.
 *
 *  **A `provider_id/model_id` id (opencode, #722) is named by its model half.**
 *  The provider is dropped from the NAME, not from the id: `modelLabel` renders
 *  `<id> — <name>`, so the id in front already carries `opencode/` verbatim and
 *  repeating it in the name is width spent on nothing. The split is deliberately
 *  narrow (one `/`, a lower-case identifier in front, a model half the
 *  prettifier can actually improve) — anything else falls through untouched
 *  rather than being half-rewritten.
 *
 *  **Returns the id untouched when it is not a plain hyphenated id** — a Bedrock
 *  inference-profile ARN, a gateway deployment name, anything with a `:` or
 *  dotted namespace. Those are identifiers, not names: reformatting one would
 *  make it wrong rather than pretty. */
export function prettyModelId(id: string): string {
  const raw = id.trim();
  if (!raw) return "";
  const slash = raw.indexOf("/");
  if (slash !== -1) {
    const provider = raw.slice(0, slash);
    const model = raw.slice(slash + 1);
    // A second `/` means this is not the two-part form (nor anything else this
    // function can name), so it stays an identifier.
    if (!isProviderId(provider) || !model || model.includes("/")) return raw;
    const pretty = prettyModelId(model);
    // The model half being unimprovable is the whole id being unimprovable —
    // handing back a name that had only lost the provider off the front would be
    // a mangle, which is the one outcome this function must never produce. The
    // comparison is case-insensitive for the same reason `modelLabel`'s is:
    // `opencode/auto` → `Auto` is a re-casing, and a name that only re-cases the
    // id has not earned the space it takes.
    return pretty.toLowerCase() === model.toLowerCase() ? raw : pretty;
  }
  const tokens = raw.split("-");
  if (!tokens.every((t) => isWordToken(t) || isVersionToken(t) || isVeeVersionToken(t))) return raw;
  const out: string[] = [];
  for (const t of tokens) {
    if (isVeeVersionToken(t)) {
      // Its own word: `deepseek-v4-flash-free` is "DeepSeek V4 Flash Free", not
      // "DeepSeek.V4" — the `v` is already the separator the vendor wrote.
      out.push(`V${t.slice(1)}`);
      continue;
    }
    if (isVersionToken(t)) {
      const prev = out[out.length - 1];
      // A version following a version is one version cut at a hyphen
      // (`claude-opus-4-8`); a version following an acronym binds tight
      // (`gpt-5.2`). Anything else is a separate word.
      if (prev !== undefined && isVersionToken(prev.split(/[ -]/).pop() ?? "")) {
        out[out.length - 1] = `${prev}.${t}`;
      } else if (prev !== undefined && prev === prev.toUpperCase() && /[A-Z]/.test(prev)) {
        out[out.length - 1] = `${prev}-${t}`;
      } else {
        out.push(t);
      }
      continue;
    }
    const lower = t.toLowerCase();
    out.push(
      ACRONYMS.has(lower)
        ? lower.toUpperCase()
        : (WORD_CASINGS[lower] ?? lower.charAt(0).toUpperCase() + lower.slice(1))
    );
  }
  return out.join(" ");
}

/** The dropdown label for a model id on a given CLI: the id, plus a name only
 *  when the name says something the id doesn't. `""` in, `""` out. */
export function modelLabel(cli: string, id: string): string {
  const raw = id.trim();
  if (!raw) return "";
  const described = ALIAS_DESCRIPTIONS[cli.trim().toLowerCase()]?.[raw.toLowerCase()];
  if (described) return `${raw} — ${described}`;
  const pretty = prettyModelId(raw);
  // Case-insensitive, and NOT separator-insensitive: `auto`/`Auto` is noise, but
  // `claude-sonnet-4.6`/`Claude Sonnet 4.6` is the whole point of the exercise.
  return !pretty || pretty.toLowerCase() === raw.toLowerCase() ? raw : `${raw} — ${pretty}`;
}

/** What the picker shows for the curated entry whose id is EMPTY — the "send no
 *  `--model` at all" option (`orchclis.INHERIT_MODEL`, #722). It is a real menu
 *  entry, so it needs real text: an option rendered from `modelLabel("")` would
 *  be a blank line the human has to guess at.
 *
 *  Deliberately not folded into `modelLabel`, whose `"" in → "" out` contract is
 *  the right answer to "what is this id called": an empty id has no name, and
 *  every OTHER caller asking that question wants nothing back rather than a
 *  sentence. This function answers a different question — what a dropdown ROW
 *  should read — which is why the two are separate. */
export const INHERIT_MODEL_LABEL = "(none) — the model your own CLI config selects";

/** What a WORKFLOW BLOCK's picker shows for the empty id (#935).
 *
 *  Deliberately NOT `INHERIT_MODEL_LABEL`: a block's missing `model:` is not
 *  "send no `--model`" on every CLI. `model_of` (workflow.rs) resolves it to
 *  `default_model(cli, kind)`, which is `sonnet`/`opus` on claude, `auto` on
 *  copilot and `pro` on gemini — only on opencode is it genuinely nothing. So the
 *  row names the RULE (whose default it is) rather than one CLI's outcome, which
 *  is the only phrasing true of all four. */
export const BLOCK_DEFAULT_MODEL_LABEL = "(unset) — orrerix's default for this block's kind and CLI";

/** The label for one row of a curated model list: `INHERIT_MODEL_LABEL` for the
 *  empty id, `modelLabel` for every real one. */
export function modelOptionLabel(cli: string, id: string): string {
  return id.trim() === "" ? INHERIT_MODEL_LABEL : modelLabel(cli, id);
}

/** The label for a row the CLI itself reported on (#993).
 *
 *  A reported `displayName` outranks everything above it, and the ordering is
 *  the point rather than a preference: `prettyModelId` is a FORMATTER that
 *  knows no model, and `ALIAS_DESCRIPTIONS` is a page this repo quoted at a
 *  point in time. A name the human's own install printed is neither — it is the
 *  thing the CLI's `/model` picker would show them, which is the name they will
 *  go looking for. The rule from the module note holds unchanged: the raw id is
 *  never lost, and a name that only re-cases the id earns no space.
 *
 *  `null` detail (nobody detected, or the reply did not mention this id) falls
 *  straight through to {@link modelOptionLabel}, which is every caller's
 *  behaviour before a human asks. */
export function detectedModelOptionLabel(cli: string, id: string, detail: ModelDetail | null): string {
  const raw = id.trim();
  if (raw === "") return INHERIT_MODEL_LABEL;
  const reported = detail?.name.trim() ?? "";
  if (!reported || reported.toLowerCase() === raw.toLowerCase()) return modelOptionLabel(cli, raw);
  return `${raw} — ${reported}`;
}

/** The one line under a picker describing the SELECTED model, or `""` when
 *  nothing is known about it (#993).
 *
 *  Three clauses, each present only when it has a source, so the line states
 *  facts and never pads:
 *
 *    the CLI's own description   reported prose, shown verbatim — loomux never
 *                                parses a number out of it, however temptingly
 *                                "Opus 5 with 1M context" reads.
 *    the effort levels it listed only when the reply listed them.
 *    the context window          from `modelcontext.ts`, which cites a page.
 *
 *  **The window is looked up against `resolvedModel` when the CLI reported one,
 *  and against the picked id otherwise — never both.** `resolvedModel` is the
 *  canonical wire id an alias resolves to on *this* install (`ModelInfo`), so
 *  `sonnet` → `claude-sonnet-5` turns a moving alias into the exact model the
 *  account is being served: the one id a static table can be sure about. When
 *  the field is absent — Anthropic documents it as requiring Claude Code
 *  v2.1.197 or later, so an older install simply omits it — there is nothing
 *  more specific than the picked id, and that is what gets looked up.
 *
 *  **Which condition is tested matters, and getting it wrong re-opened the hole
 *  the table exists to close (#997 review).** The first cut branched on the
 *  *label* being empty rather than on the *field* being absent, so a reported
 *  `resolvedModel` that the table has no row for fell through to the alias and
 *  printed the alias's number for it: `sonnet` resolving to `claude-sonnet-4-5`
 *  showed "1M context", which is precisely the inheritance
 *  `modelcontext.ts` rule 3 forbids and `test/modelcontext.test.ts` pins
 *  ("an undocumented version must not inherit its family's number"). A Bedrock
 *  or gateway `resolvedModel` did the same. Absent and unknown are different
 *  states: absent means "ask the id instead", unknown means "say nothing", and
 *  a resolved id loomux cannot place is the *more specific* statement, so its
 *  silence is the honest answer rather than a reason to consult a vaguer one. */
export function modelSummaryLine(cli: string, id: string, detail: ModelDetail | null): string {
  const parts: string[] = [];
  const described = detail?.description.trim() ?? "";
  if (described) parts.push(described);
  if (detail?.effortLevels.length) parts.push(`effort: ${detail.effortLevels.join(", ")}`);
  const resolved = detail?.resolvedId.trim() ?? "";
  // Ternary, not `||`: the fallback is for an ABSENT field, never for an
  // unknown model.
  const window = resolved ? contextWindowLabel(cli, resolved) : contextWindowLabel(cli, id);
  if (window) parts.push(window);
  return parts.join(" · ");
}

// How many tokens fit in one model's context window (#993).
//
// DOM-free and I/O-free, so `test/modelcontext.test.ts` can pin it directly.
//
// **Why a table exists here at all, when #329 says not to keep one.** Every
// other model fact loomux shows is read out of an artifact: the ids come from
// the CLI's own enumeration (`cliprobe.rs`), the effort levels come from the
// CLI's own `list_models` reply (`modelwire.ts`), the alias descriptions come
// from a vendor reference page quoted in `modelnames.ts`. The context window is
// the one number no artifact carries: the CLIs emit it only as banner prose
// ("Opus (1M context)"), there is no `--context-window` to parse, and the one
// machine-readable source — Anthropic's Models API `max_input_tokens` — needs an
// API key loomux does not have and would not ask for (and wiring one vendor's
// API into a generic tool is the host special-casing constraint 8 forbids). So
// the choice is a small maintained table or no number at all, and #993 asks for
// the number.
//
// Three rules keep the table from aging the way #329 warns about:
//
//  1. **Keyed by CLI, then by id.** An alias means what the CLI that documents
//     it says it means and nothing on any other CLI (#687, the rule
//     `modelnames.ts`' `ALIAS_DESCRIPTIONS` is already built on). `sonnet` on a
//     copilot row gets no window here, because GitHub's reference does not say
//     which Sonnet it serves or what window it serves it with, and borrowing
//     Anthropic's number would be loomux inventing a vendor fact.
//  2. **Family aliases first.** `sonnet` is the row that matters: an alias is
//     defined as "the latest Sonnet model", so its window stays right across a
//     release that a pinned `claude-sonnet-4-6` row would not. Versioned ids get
//     rows only where the vendor documents that exact model today.
//  3. **Silence is an answer.** An id with no row returns `null` and the
//     surfaces show nothing — never a guess, and never a family's number
//     applied to a version that might not share it. `claude-sonnet-4-5` is the
//     case that rule is for: it is a Sonnet, and its window is NOT the one the
//     `sonnet` alias resolves to today.

/** Tokens, per vendor documentation. */
const M = 1_000_000;
const K = 1_000;

/** Context windows by CLI, then by model id.
 *
 *  Claude Code (Anthropic models overview,
 *  <https://platform.claude.com/docs/en/about-claude/models/overview>, read
 *  2026-08-14 per the `agent-cli-reference` discipline): Claude Fable 5, Opus 5,
 *  Opus 4.8, Opus 4.7, Opus 4.6, Sonnet 5 and Sonnet 4.6 each carry a 1M
 *  context window; Claude Haiku 4.5 carries 200K.
 *
 *  The alias rows follow from Claude Code's own alias table (model-config
 *  §Model aliases, quoted in `modelnames.ts`): `sonnet` "uses the latest Sonnet
 *  model", `opus` the latest Opus, `haiku` "the fast and efficient Haiku
 *  model", `fable` Claude Fable 5. `opusplan` runs opus then sonnet, and both
 *  halves are 1M today, so the row is the same number rather than a claim about
 *  which half you are in.
 *
 *  Deliberately absent: `best` and `default`, which the vendor documents as
 *  resolving **per account** — there is no model to look a window up for, which
 *  is the same reason `selectorknobs.ts` refuses them the `[1m]` suffix. Also
 *  absent: every other CLI. Copilot's, gemini's and opencode's references state
 *  window sizes for none of the ids they offer, so loomux states none.
 *  Adding a vendor is adding rows, with the page that says so cited beside
 *  them. */
const CONTEXT_WINDOWS: Record<string, Record<string, number>> = {
  claude: {
    // Family aliases — the rows that survive a release.
    fable: M,
    opus: M,
    opusplan: M,
    sonnet: M,
    haiku: 200 * K,
    // Full model names the vendor documents today.
    "claude-fable-5": M,
    "claude-opus-5": M,
    "claude-opus-4-8": M,
    "claude-opus-4-7": M,
    "claude-opus-4-6": M,
    "claude-sonnet-5": M,
    "claude-sonnet-4-6": M,
    "claude-haiku-4-5": 200 * K,
  },
};

/** The documented meaning of the `[1m]` alias suffix: "a 1 million token
 *  context window" (model-config §Extended context, quoted in
 *  `selectorknobs.ts`). It is a property of the SUFFIX, not of the family, which
 *  is why it is applied before the table is consulted — a model whose base row
 *  says 200K but which was launched with `[1m]` is running the 1M window, and
 *  the suffix is the more specific statement.
 *
 *  Whether the suffix is *legal* on a given model is `contextModelState`'s
 *  question, not this module's. Here the id has already been chosen; reporting a
 *  different window than the flag on the wire asks for would be reporting
 *  loomux's opinion rather than the pane's configuration. */
const ONE_M_SUFFIX = "[1m]";

/** Normalize an id to the form the table is keyed on: trimmed, lower-cased, and
 *  with an opencode-style `provider_id/model_id` reduced to its model half.
 *
 *  The provider is dropped for the same reason `prettyModelId` drops it — the
 *  window is a property of the model, and `anthropic/claude-sonnet-4-5` is the
 *  same model however it is routed. The split is narrow on purpose: only one
 *  `/`, so a Bedrock ARN (several segments, `:` separators) is left whole and
 *  therefore simply misses the table, which is the honest outcome for an
 *  identifier loomux cannot resolve to a model. */
function normalizeId(id: string): string {
  const raw = id.trim().toLowerCase();
  const slash = raw.indexOf("/");
  if (slash === -1) return raw;
  const model = raw.slice(slash + 1);
  return model.includes("/") ? raw : model;
}

/** The context window in tokens for `id` on `cli`, or `null` when loomux has no
 *  documented number for it.
 *
 *  `null` is the common case and the safe one: a model newer than this build, a
 *  Bedrock ARN, a gateway deployment name, anything on a CLI whose vendor
 *  documents no window. Callers render nothing rather than a guess. */
export function contextWindowFor(cli: string, id: string): number | null {
  const raw = id.trim().toLowerCase();
  if (!raw) return null;
  if (raw.endsWith(ONE_M_SUFFIX)) return M;
  const table = CONTEXT_WINDOWS[cli.trim().toLowerCase()];
  if (!table) return null;
  return table[normalizeId(raw)] ?? null;
}

/** `1000000` → `"1M"`, `200000` → `"200K"`, `128000` → `"128K"`.
 *
 *  Rounded only where the rounding is exact: a window that is not a whole
 *  number of millions or thousands is written out in full rather than shown as
 *  a number it is not. Vendors write these as round figures, so the long form
 *  is the branch that should never fire — and if one day it does, a reader sees
 *  the real number instead of a quietly wrong abbreviation. */
export function formatTokens(tokens: number): string {
  if (tokens % M === 0) return `${tokens / M}M`;
  if (tokens % K === 0) return `${tokens / K}K`;
  return String(tokens);
}

/** The context-window phrase for a picker row or a pane badge, or `""` when
 *  there is no documented window. Empty means "say nothing", never "say zero". */
export function contextWindowLabel(cli: string, id: string): string {
  const tokens = contextWindowFor(cli, id);
  return tokens === null ? "" : `${formatTokens(tokens)} context`;
}

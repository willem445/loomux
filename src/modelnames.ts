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

const isVersionToken = (t: string): boolean => /^\d+(\.\d+)*$/.test(t);
const isWordToken = (t: string): boolean => /^[A-Za-z]+$/.test(t);

/** `claude-sonnet-4.6` → `Claude Sonnet 4.6`, `gpt-5.3-codex` → `GPT-5.3 Codex`.
 *
 *  Structural only: it splits on `-`, title-cases words, upper-cases known
 *  acronyms, and rejoins version segments (`4-8` → `4.8`, the way Anthropic
 *  writes the model it names). It knows no model, so it cannot go stale.
 *
 *  **Returns the id untouched when it is not a plain hyphenated id** — a Bedrock
 *  inference-profile ARN, a gateway deployment name, anything with a `:` or
 *  dotted namespace. Those are identifiers, not names: reformatting one would
 *  make it wrong rather than pretty. */
export function prettyModelId(id: string): string {
  const raw = id.trim();
  if (!raw) return "";
  const tokens = raw.split("-");
  if (!tokens.every((t) => isWordToken(t) || isVersionToken(t))) return raw;
  const out: string[] = [];
  for (const t of tokens) {
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
    out.push(ACRONYMS.has(lower) ? lower.toUpperCase() : lower.charAt(0).toUpperCase() + lower.slice(1));
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

// The agent-type mark: which CLI is running in this pane (#992).
//
// WHY THIS MODULE EXISTS. A supervisor watching ten panes can already tell WHICH AGENT a
// pane is (the `W 2` / `REV 3` role badge, dyed by group) but not WHICH PROGRAM is running
// inside it. That answer is in the pane title and the launch command, i.e. it has to be
// read rather than seen, and it is the thing you most want when a group mixes CLIs — the
// workflow file pins a different CLI per block, so "the copilot one is stuck" is a sentence
// a supervisor thinks before they know which pane they mean.
//
// TWO TIERS, AND THE SECOND ONE IS THE LOAD-BEARING ONE:
//
//   1. A MARK — a vendored, licensed brand glyph for a CLI that has one (see §Licensing).
//   2. A LETTER BADGE — a rounded rect around the program's first character, generated.
//
// The table is deliberately near-empty and the fallback is deliberately total: loomux gains
// CLIs faster than brand kits gain licences, and a fixed set of three would mean the fourth
// CLI a user launches renders as nothing at all. So the resolver never asks "is this one of
// ours" — it asks "does this program have a mark, and if not, what letter is it". Adding a
// CLI is a table row when a licensed glyph exists and NO CHANGE AT ALL when it doesn't.
//
// WHAT IT REFUSES TO DO. It never guesses. ORCA (the prior art in #992) coerced an
// unidentified pane to the claude icon and had to undo it: a pane wearing the wrong brand
// for the second before identification lands is worse than a pane wearing none, because the
// wrong one is read as an answer. Here, no command means `null` — draw nothing — and an
// unreadable program name means `?`, which reads as "loomux does not know" and is true.
//
// DOM-free on purpose: test/agenticons.test.ts imports this directly (no jsdom, no
// bundler), exactly like src/icons.ts. The strings are injected with `innerHTML` by
// src/pane.ts; the `label` is NOT — it is text for a `title` property and must never be
// interpolated into markup (see §Safety).
import { normalizeAgentProgram, programFromRestore } from "./panerestore.ts";

/**
 * §Licensing — why the table has one row and a fallback rather than one row per CLI.
 *
 * A brand mark is TWO permissions, and vendoring one needs both:
 *
 *   * COPYRIGHT in the artwork — settled by a licence the project itself grants. Octicons
 *     is MIT, from GitHub, over GitHub's own Copilot glyph, so redistribution inside
 *     loomux's bundle is granted outright and the notice below discharges the condition.
 *   * TRADEMARK in the mark — never granted by an OSS licence and never needed here: using
 *     GitHub's Copilot mark to label a pane that is running GitHub Copilot is nominative
 *     use, the one thing a mark exists for. Loomux claims no affiliation, sells nothing on
 *     the strength of it, and — the part that matters operationally — does not MODIFY the
 *     artwork: the bodies below are the upstream paths verbatim, and they carry no fill of
 *     their own, so `currentColor` on the wrapper is the same single-colour rendering
 *     upstream ships (GitHub's logo guidelines allow a one-colour mark; they forbid
 *     redrawing one).
 *
 * A CLI whose vendor publishes no such grant — Anthropic's Claude and opencode today —
 * therefore gets the letter badge, NOT a traced lookalike and NOT a third-party
 * aggregator's copy. An aggregator's CC0 covers the aggregator's tracing, which is not the
 * permission at issue and is worth exactly nothing against the trademark; a hand-traced
 * lookalike is a derivative of the mark with no grant behind it at all, which is the
 * practice #992 was written to avoid. The honest badge is the cheaper answer and it is
 * still legible at a glance — the shape says "loomux has no licensed mark for this", the
 * tooltip says which program it is.
 *
 * If a vendor later publishes a licensed glyph, it lands as a row here, a pin, a file under
 * `src/vendor/`, and an entry in THIRD_PARTY_NOTICES.md — the same four surfaces the Lucide
 * vendoring uses, and test/agenticons.test.ts fails if they disagree.
 */
export const OCTICONS_PIN = {
  repo: "https://github.com/primer/octicons",
  version: "19.33.0",
  commit: "cc4e12df6ff8292447ba9141eaa2a6f6e1c59a85",
  license: "MIT",
} as const;

/** A vendored brand glyph: upstream's grid and upstream's paths, nothing else. */
interface Mark {
  /** The upstream icon's own name, e.g. `copilot-16` — what a re-vendor fetches, and what
   *  the licence paperwork has to name. test/agenticons.test.ts pins it against both. */
  upstream: string;
  /** Upstream's own viewBox — never rescaled, so the geometry stays the vendor's. */
  viewBox: string;
  /** The inner markup of the upstream `.svg`, verbatim. */
  body: string;
}

/**
 * program name (as `normalizeAgentProgram` spells it) → its licensed mark.
 *
 * One row, and that is the point rather than an oversight — see §Licensing. Everything
 * absent from this table renders as a letter badge, including the CLIs loomux ships
 * first-class support for.
 *
 * NULL PROTOTYPE, and it is load-bearing rather than a flourish. The keys here are PROGRAM
 * NAMES taken from a launch line, so `MARK[program]` is an attacker-adjacent lookup on a
 * dictionary: with an ordinary object literal, `MARK["constructor"]` and `MARK["__proto__"]`
 * resolve to inherited members, come back TRUTHY, and take the "this program has a licensed
 * mark" branch with `viewBox` and `body` undefined — an empty box that asserts loomux has a
 * brand mark it does not have, and a small hole in the claim that the letter fallback is
 * total (#992 review NB2). `normalizeAgentProgram` lowercases, so only those two all-lowercase
 * prototype members can reach it, but "only two" is not "none".
 *
 * Fixed at the STRUCTURE rather than with a `hasOwn` guard at the one read site: a guard
 * protects the read that remembers to carry it, and the next reader of this table — a
 * settings surface listing which CLIs have marks, say — would inherit nothing. A table with
 * no prototype has nothing to inherit, so every present and future lookup is safe by
 * construction. `Object.keys`/`Object.entries` work on it unchanged.
 */
const MARK: Record<string, Mark> = Object.assign(Object.create(null) as Record<string, Mark>, {
  // Primer Octicons `copilot-16` @ OCTICONS_PIN, copied verbatim (MIT). Filled paths with
  // no `fill` of their own, so they take the wrapper's `currentColor`.
  copilot: {
    upstream: "copilot-16",
    viewBox: "0 0 16 16",
    body:
      `<path d="M7.998 15.035c-4.562 0-7.873-2.914-7.998-3.749V9.338c.085-.628.677-1.686 1.588-2.065.013-.07.024-.143.036-.218.029-.183.06-.384.126-.612-.201-.508-.254-1.084-.254-1.656 0-.87.128-1.769.693-2.484.579-.733 1.494-1.124 2.724-1.261 1.206-.134 2.262.034 2.944.765.05.053.096.108.139.165.044-.057.094-.112.143-.165.682-.731 1.738-.899 2.944-.765 1.23.137 2.145.528 2.724 1.261.566.715.693 1.614.693 2.484 0 .572-.053 1.148-.254 1.656.066.228.098.429.126.612.012.076.024.148.037.218.924.385 1.522 1.471 1.591 2.095v1.872c0 .766-3.351 3.795-8.002 3.795Zm0-1.485c2.28 0 4.584-1.11 5.002-1.433V7.862l-.023-.116c-.49.21-1.075.291-1.727.291-1.146 0-2.059-.327-2.71-.991A3.222 3.222 0 0 1 8 6.303a3.24 3.24 0 0 1-.544.743c-.65.664-1.563.991-2.71.991-.652 0-1.236-.081-1.727-.291l-.023.116v4.255c.419.323 2.722 1.433 5.002 1.433ZM6.762 2.83c-.193-.206-.637-.413-1.682-.297-1.019.113-1.479.404-1.713.7-.247.312-.369.789-.369 1.554 0 .793.129 1.171.308 1.371.162.181.519.379 1.442.379.853 0 1.339-.235 1.638-.54.315-.322.527-.827.617-1.553.117-.935-.037-1.395-.241-1.614Zm4.155-.297c-1.044-.116-1.488.091-1.681.297-.204.219-.359.679-.242 1.614.091.726.303 1.231.618 1.553.299.305.784.54 1.638.54.922 0 1.28-.198 1.442-.379.179-.2.308-.578.308-1.371 0-.765-.123-1.242-.37-1.554-.233-.296-.693-.587-1.713-.7Z" />` +
      `<path d="M6.25 9.037a.75.75 0 0 1 .75.75v1.501a.75.75 0 0 1-1.5 0V9.787a.75.75 0 0 1 .75-.75Zm4.25.75v1.501a.75.75 0 0 1-1.5 0V9.787a.75.75 0 0 1 1.5 0Z" />`,
  },
});

/** Every program that has a licensed mark, for tests and for anything that wants the set. */
export const MARK_PROGRAMS = Object.keys(MARK);

/** program → the upstream icon name its artwork was vendored from. The licence paperwork
 *  has to name every one of these; the test checks that it does. */
export const MARK_SOURCES: Record<string, string> = Object.fromEntries(
  Object.entries(MARK).map(([program, m]) => [program, m.upstream])
);

/**
 * §The dye — which CLI, in colour, alongside which CLI in shape.
 *
 * Every mark used to be violet: it took the `fleet` icon role's dye, which answers *is this
 * an agent* and says nothing about *which one* — the question this whole module exists for.
 * With three CLIs on screen the shape was doing all the work, and the shape is a single
 * letter for most of them.
 *
 * So a mark whose program is on this roster wears `cli-<program>` and takes that program's
 * pigment; a mark whose program is not wears `ic-fleet` and keeps the violet, which reads as
 * "an agent loomux has no brand hue for" — the colour twin of the letter badge's own total
 * fallback, and the same refusal to guess (§the module header). EITHER, NEVER BOTH: a mark
 * carrying both classes would need one CSS rule to out-specify the other, and that pin would
 * hold only while the two blocks stayed in their current source order in styles.css.
 *
 * THE LIST, NOT THE HUES. This module still never learns a colour (§the header's "colour is
 * assignment, not asset"): it knows only which programs HAVE one. The pigments live in
 * theme.ts's `CLI_HUES` and reach the mark through `.cli-<program>` in styles.css, and
 * test/agenticons.test.ts pins all three surfaces against each other so the roster cannot
 * drift into a class nothing dyes or a token nothing stamps.
 *
 * A CLOSED ROSTER IS ALSO THE INJECTION ANSWER. `program` comes off a launch line, and this
 * value lands in a `class` attribute inside an `innerHTML` string — the same untrusted byte
 * the letter badge clamps to one character (§Safety). Interpolating it would put
 * `"><img onerror=…>` straight into the markup. It cannot: only a name that MATCHES one of
 * the literals below is ever interpolated, so what reaches the attribute is one of seven
 * compile-time strings and there is nothing to escape. Same discipline as the clamp — make
 * the hostile value unexpressible rather than escaped.
 */
export const CLI_DYE_PROGRAMS = [
  "claude",
  "codex",
  "copilot",
  "opencode",
  "gemini",
  "hermes",
  "ante",
] as const;

/** Membership test for the roster above. A `Set`, so a program named after a prototype
 *  member (`constructor`, `__proto__`) answers `false` like everything else — the same hole
 *  `MARK`'s null prototype closes, closed the same way at the structure. */
const CLI_DYED = new Set<string>(CLI_DYE_PROGRAMS);

/**
 * The ONE colour class a mark wears: its CLI's, or the fleet role's.
 *
 * Exported because the pane-setup preview and the header render the same view object, and a
 * test that wants to ask "what dyes this mark" should not have to parse an SVG string.
 */
export function cliDyeClass(program: string | null): string {
  return program !== null && CLI_DYED.has(program) ? `cli-${program}` : "ic-fleet";
}

/**
 * The letter badge's grid. Sixteen, not the registry's twenty-four, because the badge is
 * drawn to sit beside `copilot-16` — the two tiers have to look like one channel, and the
 * one thing that guarantees that is sharing a grid with the marks rather than with
 * src/icons.ts. (A glyph cannot be both: `icon()` pins every body to `ICON_VIEWBOX`, which
 * is exactly why the agent marks are a separate module rather than rows in that registry.)
 */
export const AGENT_VIEWBOX = "0 0 16 16";

/** The box the pane header draws these in — the toolbar buttons' 13px, so the mark sits on
 *  the header's existing optical line rather than introducing a third icon size. */
export const ICON_AGENT_PX = 13;

/**
 * §Safety — the fallback's letter is the only user-controlled byte that reaches the SVG.
 *
 * `program` comes from a launch command, which a human types and a workflow file supplies,
 * so it is not markup-safe: `<script>x</script>` normalizes to a perfectly ordinary-looking
 * program name. Rather than escape it, this clamps it — one character, and only if it is
 * `[A-Z0-9]`. Nothing else can be expressed, so there is nothing to escape, and a name that
 * starts with punctuation or a non-Latin letter yields `?` rather than a mangled glyph.
 *
 * `?` is also the answer for the genuinely unknown, and that overlap is deliberate: both
 * cases mean "loomux cannot name this program", which is what the reader needs to know.
 */
export function agentLetter(program: string): string {
  const c = program.trim().charAt(0).toUpperCase();
  return /^[A-Z0-9]$/.test(c) ? c : "?";
}

/** What a pane header needs in order to draw the mark. */
export interface AgentMarkView {
  /** The normalized program name this mark NAMES, e.g. `claude`, `copilot`, `aider` — and
   *  `null` on the neutral tier, where the whole point is that there is no name to give. */
  program: string | null;
  /** `mark` = a vendored licensed glyph; `letter` = the generated fallback;
   *  `unknown` = the neutral badge, which asserts nothing about which CLI this is. */
  kind: "mark" | "letter" | "unknown";
  /** Inline SVG, `currentColor`, safe for `innerHTML`. */
  svg: string;
  /** Tooltip TEXT — assign it to `.title`/`aria-label`, never interpolate it into markup.
   *  Only a `mark`/`letter` view captions itself "Agent CLI: …"; a neutral one must not,
   *  because that caption is a claim. */
  label: string;
}

/** Tooltips are chrome, not a log line: a pathological command should not produce a
 *  tooltip the width of the screen. Program names are basenames, so this never bites a
 *  real one. */
const LABEL_MAX = 24;

/**
 * §Not every program in a launch line is an agent.
 *
 * A **denylist of transports and shells**, and the direction matters: an allowlist of
 * known agents would destroy the total fallback that makes the letter tier worth having
 * (§the module header), so this only ever removes things that definitionally cannot be an
 * agent CLI, whatever they were launched by.
 *
 * `ssh` is the one that had to exist. An #887 SSH pane spawns the LOCAL ssh client as the
 * pane's child, so `argv[0]` is the transport — and the agent, if any, is running on the
 * far end where argv cannot see it. Reading `ssh` as the pane's CLI produced a confident,
 * specific, wrong answer ("Agent CLI: ssh") on a pane that may well have been running
 * Claude, which is the exact failure this module's header claims it does not have. The
 * shells are here for the same reason one step down: a pane launched with `bash` is a
 * shell someone opened, not an agent that happens to be named bash.
 */
const NOT_AN_AGENT = new Set([
  "ssh",
  "mosh",
  "bash",
  "sh",
  "zsh",
  "fish",
  "dash",
  "cmd",
  "powershell",
  "pwsh",
  "wsl",
  "tmux",
  "screen",
]);

/** The neutral tier: "loomux does not know which agent this is", drawn as `?` and — the
 *  load-bearing part — captioned WITHOUT an "Agent CLI:" claim. */
function neutralView(label: string, size: number): AgentMarkView {
  // `null` program ⇒ the fleet dye. A neutral badge must not borrow a CLI's pigment for the
  // same reason it must not borrow a CLI's caption: both would be answering a question this
  // tier exists to decline.
  return { program: null, kind: "unknown", label, svg: badgeSvg("?", size, cliDyeClass(null)) };
}

/** What a remote pane says when loomux holds no far-end CLI for it. */
export const REMOTE_UNKNOWN_LABEL = "Remote pane — agent CLI unknown";

/** The generated badge. No `font-family`: SVG text inherits it from the header, so the
 *  letter is the app's own type rather than a second typeface nobody chose. */
function badgeSvg(letter: string, size: number, dye: string): string {
  return (
    `<svg class="ic ${dye}" viewBox="${AGENT_VIEWBOX}" width="${size}" height="${size}" ` +
    `aria-hidden="true">` +
    `<rect x="1.25" y="1.25" width="13.5" height="13.5" rx="4" fill="none" ` +
    `stroke="currentColor" stroke-width="1.5" />` +
    `<text x="8" y="11.4" text-anchor="middle" font-size="9" font-weight="700" ` +
    `fill="currentColor">${letter}</text>` +
    `</svg>`
  );
}

/**
 * Resolve a program name to its mark. Total — every string gets a view, because the
 * fallback is the whole point (see the module header).
 */
export function agentMarkFor(program: string, size = ICON_AGENT_PX): AgentMarkView {
  // Transports and shells fall out here rather than at the call site, so EVERY route into
  // this function — a launch line, an SSH profile's `defaultCli`, anything later — gets the
  // same refusal. A caption is a claim; these have nothing to claim.
  if (NOT_AN_AGENT.has(program)) {
    return neutralView(`${program.slice(0, LABEL_MAX)} — a transport or shell, not an agent`, size);
  }

  const label = `Agent CLI: ${program.slice(0, LABEL_MAX)}`;

  const mark = MARK[program];
  if (mark) {
    // Upstream's viewBox, not AGENT_VIEWBOX, so a future mark drawn on another grid keeps
    // its own geometry instead of being silently stretched onto this one.
    return {
      program,
      kind: "mark",
      label,
      svg:
        `<svg class="ic ${cliDyeClass(program)}" viewBox="${mark.viewBox}" ` +
        `width="${size}" height="${size}" ` +
        `fill="currentColor" aria-hidden="true">${mark.body}</svg>`,
    };
  }

  // A name whose first character cannot be badged is a name loomux could not read, so it
  // resolves to the neutral tier rather than to a `?` wearing an "Agent CLI:" caption —
  // the caption would be asserting exactly what the `?` is admitting it does not know.
  const letter = agentLetter(program);
  if (letter === "?") return neutralView("Agent CLI not identified", size);

  return { program, kind: "letter", label, svg: badgeSvg(letter, size, cliDyeClass(program)) };
}

/** Everything the resolver is allowed to know about a pane. An object rather than
 *  positional arguments because the two new fields are the interesting ones and a
 *  `agentMark(cmd, argv, cli, true)` call site would hide which is which. */
export interface AgentMarkInput {
  /** The pane's launch command string, if it has one. */
  command?: string | null;
  /** The pane's launch argv, if it has one. */
  argv?: string[] | null;
  /**
   * The CLI loomux ALREADY KNOWS this pane runs, from somewhere better than the launch
   * line — today an SSH profile's `defaultCli` (`src/sshprofile.ts`), which is the name
   * `sshLaunchParams` actually composed the remote command from.
   *
   * **Authoritative: it beats anything inferable from `command`/`argv`.** For an SSH pane
   * the launch line describes the TRANSPORT and this describes the AGENT, so preferring
   * the launch line would be preferring the answer we know to be about something else.
   */
  knownCli?: string | null;
  /**
   * True when the launch line is a transport rather than the agent — an #887 SSH pane.
   *
   * Separate from `knownCli` because the two absences mean different things: no `knownCli`
   * on a local pane means "read the launch line", and no `knownCli` on a REMOTE pane means
   * "the answer is on the far end and loomux does not have it" — which must not fall
   * through to reading the transport's own name.
   */
  remote?: boolean;
}

/**
 * The pane's question: what mark does this pane deserve?
 *
 * Resolution order, and each step exists because the one below it would otherwise lie:
 *
 *   1. `knownCli` — the authoritative answer, when loomux holds one.
 *   2. `remote` with no `knownCli` — the neutral badge. NOT the launch line: `argv[0]` of an
 *      SSH pane is the local ssh client, and captioning a pane "Agent CLI: ssh" while it
 *      runs Claude on the far end is precisely the confident-wrong-answer this module's
 *      header claims it does not produce.
 *   3. no launch line at all — `null`, DRAW NOTHING. A plain shell is not an agent, and a
 *      row of `?` badges over every terminal is noise dressed as information.
 *   4. otherwise the launch line, through `agentMarkFor` (which still refuses transports
 *      and shells, so a hand-typed `ssh …` command pane lands neutral too).
 *
 * The program name comes from `programFromRestore`, deliberately rather than from a second
 * first-token parse here: that function is the one place that answers "what program does
 * this raw token name" (#452), and it already handles a path-qualified, `.exe`-suffixed or
 * argv-only command. A private copy would be a fourth derivation, and it would be the one
 * that quietly disagrees on Windows.
 */
export function agentMark(input: AgentMarkInput, size = ICON_AGENT_PX): AgentMarkView | null {
  const known = input.knownCli?.trim();
  if (known) return agentMarkFor(normalizeAgentProgram(known), size);
  if (input.remote) return neutralView(REMOTE_UNKNOWN_LABEL, size);
  const program = programFromRestore(input.command ?? null, input.argv ?? null);
  return program ? agentMarkFor(program, size) : null;
}

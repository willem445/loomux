// The per-agent pane mark (#992).
//
// The feature's whole claim is "tell which CLI a pane is running WITHOUT reading it", and
// three things can quietly break that claim in ways a screenshot would not show:
//
//   * the resolver stops being total — a CLI nobody has heard of renders as nothing, so the
//     pane it most matters for is the one with no mark;
//   * it starts guessing — an unidentified pane wears some brand's glyph, which is worse
//     than no glyph because a reader takes it as an answer (the ORCA regression #992 cites);
//   * the letter fallback stops being a clamp — the program name is a launch command, so an
//     unclamped fallback puts human-typed text straight into `innerHTML` in a webview that
//     can reach the Tauri IPC bridge.
//
// Plus the paperwork: a vendored brand mark is only redistributable while its licence,
// its pin and its notice all say the same thing.
//
// Run `npm test`.
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import {
  AGENT_VIEWBOX,
  CLI_DYE_PROGRAMS,
  ICON_AGENT_PX,
  MARK_PROGRAMS,
  MARK_SOURCES,
  OCTICONS_PIN,
  REMOTE_UNKNOWN_LABEL,
  agentLetter,
  agentMark,
  agentMarkFor,
} from "../src/agenticons.ts";
import { buildSshArgv } from "../src/sshcommand.ts";
import { ROLE_TOKEN } from "../src/icons.ts";
import { AGENTS } from "../src/agents.ts";
import { CLI_HUES, CSS_TOKENS } from "../src/theme.ts";

const read = (rel: string) => readFileSync(new URL(rel, import.meta.url), "utf8");

/** The markup between the wrapper's `<svg …>` and `</svg>`. */
function body(svg: string): string {
  const m = svg.match(/^<svg\b[^>]*>([\s\S]*)<\/svg>$/);
  assert.ok(m, `not a single well-formed <svg> element: ${svg.slice(0, 80)}…`);
  return m[1];
}

test("a pane with no launch command gets no mark at all", () => {
  // THE "NEVER GUESS" HALF. A shell pane is not an agent pane, and a `?` badge on every
  // plain terminal is noise dressed as information — so the absence of a command has to
  // resolve to *nothing*, not to a neutral glyph and certainly not to a default CLI.
  for (const [command, argv] of [
    [null, null],
    [undefined, undefined],
    ["", null],
    ["   ", null],
    [null, []],
  ] as const) {
    assert.equal(
      agentMark({ command, argv: argv as string[] | null }),
      null,
      `agentMark(${JSON.stringify(command)}, ${JSON.stringify(argv)}) drew something`
    );
  }
  // A commandless pane stays blank even when it is remote-flagged with nothing known —
  // `remote` is about where the agent RUNS, not a licence to invent one.
  assert.equal(agentMark({ command: null, argv: null, remote: false }), null);
});

/** The real #887 launch line for an SSH pane, composed by the app's own argv builder
 *  rather than hand-written here — a hand-written `["ssh", ...]` would pass this test while
 *  the shipped composer changed underneath it. `remoteCommand` is what `sshLaunchParams`
 *  builds from `profile.defaultCli`. */
function sshPaneArgv(remoteCli: string | null): string[] {
  return buildSshArgv("C:\\Windows\\System32\\OpenSSH\\ssh.exe", {
    destination: "dev@box",
    remoteShell: "posix",
    remoteCwd: "/srv/app",
    ...(remoteCli ? { remoteCommand: [remoteCli, "--session-id", "abc"] } : {}),
  });
}

test("an SSH pane never wears the transport as its agent", () => {
  // REVIEW B1. An #887 SSH pane's child process is the LOCAL ssh client, so `argv[0]` is
  // `ssh` — and the agent, if there is one, runs on the far end where argv cannot see it.
  // Reading argv[0] gave a confident, specific, WRONG answer: a violet `S` captioned
  // "Agent CLI: ssh" on a pane that may well have been running Claude. That is exactly the
  // failure this module's header claims it does not have ("a wrong mark is strictly worse
  // than no mark, because a reader takes it as an answer"), so it is pinned here.
  const argv = sshPaneArgv("claude");
  assert.equal(argv[0], "C:\\Windows\\System32\\OpenSSH\\ssh.exe", "fixture drifted");

  // 1. With the profile's far-end CLI in hand, the mark names the REAL agent.
  const known = agentMark({ argv, knownCli: "claude", remote: true });
  assert.ok(known);
  assert.equal(known.program, "claude");
  assert.equal(known.kind, "letter"); // Claude has no licensed mark — see §Licensing
  assert.match(known.label, /claude/);
  assert.doesNotMatch(known.label, /ssh/, "the transport leaked into the caption");

  // 2. Without it, the pane goes NEUTRAL — never captioned as an agent, and never `S`.
  for (const view of [
    agentMark({ argv, remote: true }),
    agentMark({ argv, knownCli: null, remote: true }),
    agentMark({ argv, knownCli: "   ", remote: true }),
    agentMark({ argv: sshPaneArgv(null), remote: true }), // a plain remote login shell
  ]) {
    assert.ok(view, "an SSH pane drew nothing at all");
    assert.equal(view.kind, "unknown", `SSH pane resolved to ${view.kind}, not the neutral tier`);
    assert.equal(view.program, null, "the neutral tier must not name a program");
    assert.equal(view.label, REMOTE_UNKNOWN_LABEL);
    assert.doesNotMatch(view.label, /Agent CLI:/, "a neutral badge must not caption a claim");
    assert.ok(view.svg.includes(">?</text>"), "the neutral badge is `?`, never a letter");
    assert.equal(view.svg.includes(">S</text>"), false, "the transport's initial is showing");
  }

  // 3. And the same refusal for a hand-typed `ssh …` command pane, which carries no
  //    `remote` flag at all — the denylist catches it on the launch-line path too.
  const typed = agentMark({ command: "ssh dev@box" });
  assert.equal(typed?.kind, "unknown");
  assert.doesNotMatch(typed!.label, /Agent CLI:/);
});

test("a CLI loomux has never heard of still gets a mark", () => {
  // THE STRUCTURAL PROPERTY, and the one a "small table of the three CLIs we support"
  // implementation would fail. loomux gains CLIs faster than vendors publish licensed
  // glyphs; if the resolver only answered for a known set, the fourth CLI a user launches
  // would be the invisible one — precisely the pane a supervisor needs to pick out.
  for (const [command, letter] of [
    ["aider --model x", "A"],
    ["gemini", "G"],
    ["codex exec", "C"],
    ["zed-agent", "Z"],
  ] as const) {
    const view = agentMark({ command });
    assert.ok(view, `${command} drew nothing`);
    assert.equal(view.kind, "letter", `${command} should fall back to a letter badge`);
    assert.ok(
      view.svg.includes(`>${letter}</text>`),
      `${command} should badge "${letter}", got: ${view.svg}`
    );
  }
});

test("copilot draws the vendored mark, and the other first-class CLIs draw letters", () => {
  // The two tiers, asserted as tiers rather than as a fixed roster: what separates copilot
  // from claude here is not "we like it more", it is that GitHub grants a licence over its
  // own glyph and Anthropic publishes none (see §Licensing in the module). If a future
  // commit hand-traces a lookalike into the table, the `letter` assertions below go red and
  // the licence question gets asked out loud instead of being decided by a paste.
  const copilot = agentMark({ command: "copilot --autopilot" });
  assert.ok(copilot);
  assert.equal(copilot.kind, "mark");
  assert.equal(copilot.program, "copilot");

  for (const program of ["claude", "opencode"]) {
    const view = agentMark({ command: program });
    assert.ok(view);
    assert.equal(
      view.kind,
      "letter",
      `${program} draws a vendored mark — which licence grants it? (module §Licensing)`
    );
  }

  // And the two letter badges are distinguishable from each other, which is the entire
  // point of drawing anything.
  assert.notEqual(agentMark({ command: "claude" })!.svg, agentMark({ command: "opencode" })!.svg);
});

test("a program named after a prototype member is not a licensed mark", () => {
  // REVIEW NB2. `MARK[program]` is a dictionary lookup keyed by a name taken off a launch
  // line, so with an ordinary object literal `MARK["constructor"]` and `MARK["__proto__"]`
  // came back TRUTHY — inherited members — and took the "this program has a licensed brand
  // mark" branch with `viewBox` and `body` undefined. The pane drew an empty box asserting
  // loomux holds a mark it does not hold, and the claim that the letter tier is TOTAL had a
  // hole in it for exactly two inputs.
  //
  // Not a security hole (the interpolated value is the literal string "undefined", and the
  // clamp is untouched), which is why it is pinned as correctness rather than as safety.
  for (const name of ["constructor", "__proto__", "valueof", "tostring", "hasownproperty"]) {
    const view = agentMarkFor(name);
    assert.notEqual(
      view.kind,
      "mark",
      `${name} resolved to a licensed mark; the lookup is reading the prototype chain`
    );
    assert.equal(view.svg.includes("undefined"), false, `${name} rendered an undefined field`);
    assert.match(view.svg, /viewBox="0 0 16 16"/, `${name} lost its grid`);
  }

  // And the same names arriving the way a user would actually deliver them.
  assert.notEqual(agentMark({ command: "constructor --go" })?.kind, "mark");
  assert.notEqual(agentMark({ command: "C:\\bin\\constructor.exe" })?.kind, "mark");
  assert.notEqual(agentMark({ knownCli: "__proto__", remote: true })?.kind, "mark");

  // The real entry still resolves — a fix that broke the table would pass everything above.
  assert.equal(agentMarkFor("copilot").kind, "mark");
  assert.deepEqual(MARK_PROGRAMS, ["copilot"], "the vendored set changed without its paperwork");
});

test("the program name is read the same way the rest of the app reads it", () => {
  // Not a re-test of `normalizeAgentProgram` — a test that this module went through it
  // (#452's single derivation) instead of parsing the first token privately. A private
  // parse looks identical on `copilot` and disagrees on every real Windows launch line,
  // which is the form loomux actually spawns.
  const forms = [
    "copilot",
    "COPILOT",
    "copilot.exe --autopilot",
    "copilot.CMD",
    "C:\\tools\\gh\\copilot.EXE --banner",
    "/usr/local/bin/copilot",
  ];
  for (const command of forms) {
    const view = agentMark({ command });
    assert.ok(view, `${command} drew nothing`);
    assert.equal(view.kind, "mark", `${command} did not resolve to the copilot mark`);
  }
  // argv-only launches (a restored pane records argv, not a command string) resolve too.
  const fromArgv = agentMark({ argv: ["copilot", "--autopilot"] });
  assert.equal(fromArgv?.kind, "mark");

  // THE INHERITED LIMIT, pinned rather than papered over. `programFromRestore` splits the
  // command on whitespace before it looks at anything else, so a command string whose
  // program path CONTAINS a space resolves to the fragment before the space — quoted or
  // not. That is the tokenizing grammar axis (#471), which this feature deliberately does
  // not touch: it is shared with the autopilot watcher and the session reconciler, which
  // already mis-read the same line today. The mark degrades to a letter badge, which is
  // the right failure — a wrong glyph would be worse — and this assertion is here so a
  // future fix to that grammar shows up as a test to update rather than as a surprise.
  const spaced = agentMark({ command: `C:\\Program Files\\gh\\copilot.exe --banner` });
  assert.equal(spaced?.kind, "letter");
  assert.equal(spaced?.program, "program");
});

test("the letter badge is a clamp, not an escape", () => {
  // The security property. `program` is derived from a launch command — human-typed, or
  // supplied by a workflow file — and the result is injected with `innerHTML`. Clamping to
  // one `[A-Z0-9]` character means there is no expressible markup to escape in the first
  // place, so this cannot regress into "we escaped the four characters we thought of".
  assert.equal(agentLetter("aider"), "A");
  assert.equal(agentLetter("7zip"), "7");
  assert.equal(agentLetter(""), "?");
  assert.equal(agentLetter("   "), "?");
  assert.equal(agentLetter("-x"), "?");
  assert.equal(agentLetter("<script>"), "?");
  assert.equal(agentLetter("émacs"), "?"); // uppercases to É, which is not [A-Z0-9]
  assert.equal(agentLetter("日本語"), "?");

  for (const hostile of [
    `<script>alert(1)</script>`,
    `"><img src=x onerror=alert(1)>`,
    `'"--><svg onload=alert(1)>`,
    `&lt;b&gt;`,
    `a" onmouseover="alert(1)`,
  ]) {
    // No `if (!view) continue` — a hostile command that resolved to nothing would make
    // every assertion below vacuous and this test would still pass while checking
    // NOTHING. Each of these launch lines does name a program, so each must produce a
    // view, and that is asserted rather than assumed.
    const view = agentMark({ command: hostile });
    assert.ok(view, `${hostile} drew nothing — the assertions below would be vacuous`);
    const inner = body(view.svg);
    assert.equal(/<script|<foreignObject|javascript:/i.test(view.svg), false, hostile);
    assert.equal(/\son[a-z]+\s*=/i.test(view.svg), false, `${hostile} carries an event handler`);
    assert.equal(/\b(href|xlink:href|src)\s*=/i.test(view.svg), false, `${hostile} references`);
    // The only text node is the single clamped character.
    const texts = [...inner.matchAll(/<text\b[^>]*>([\s\S]*?)<\/text>/g)].map((m) => m[1]);
    for (const t of texts) assert.match(t, /^[A-Z0-9?]$/, `text node "${t}" is not one clamped char`);
  }
});

test("every mark is one element, on a declared grid, in currentColor, aria-hidden", () => {
  const views = [
    agentMarkFor("copilot"),
    agentMarkFor("claude"),
    agentMarkFor("nothing-has-this-name"),
  ];
  for (const view of views) {
    assert.ok(view.svg.startsWith("<svg ") && view.svg.endsWith("</svg>"), view.program);
    assert.ok(view.svg.includes(`aria-hidden="true"`), `${view.program} is not decorative`);
    assert.ok(view.svg.includes("currentColor"), `${view.program} does not inherit its colour`);
    assert.ok(
      view.svg.includes(`width="${ICON_AGENT_PX}" height="${ICON_AGENT_PX}"`),
      `${view.program} is not drawn in the header's box`
    );
    // Exactly one viewBox, and a generated badge uses this module's own grid.
    assert.equal((view.svg.match(/viewBox=/g) ?? []).length, 1);
    if (view.kind === "letter") assert.ok(view.svg.includes(`viewBox="${AGENT_VIEWBOX}"`));
  }
  // …and a call site that asks for a different box gets it.
  assert.ok(agentMarkFor("claude", 20).svg.includes(`width="20" height="20"`));
});

test("no mark carries a colour of its own", () => {
  // Same load-bearing rule as the Lucide registry (test/icons.test.ts): a literal here
  // would be the one mark the palette cannot reach, through every theme, silently.
  const LITERAL = /#[0-9a-fA-F]{3,8}\b|\b(rgba?|hsla?|hwb|lab|lch|oklab|oklch)\s*\(/;
  for (const program of [...MARK_PROGRAMS, "claude", "unheard-of"]) {
    const svg = agentMarkFor(program).svg;
    assert.equal(LITERAL.test(svg), false, `${program} contains a colour literal`);
    for (const [, attr, value] of svg.matchAll(/\b(fill|stroke|stop-color|color)\s*=\s*"([^"]*)"/g)) {
      assert.ok(
        value === "currentColor" || value === "none",
        `${program} paints ${attr}="${value}"; only currentColor or none may appear`
      );
    }
  }
});

/** Every class on a mark's `<svg>`, in source order. */
function classesOf(svg: string): string[] {
  const cls = svg.match(/class="([^"]*)"/);
  assert.ok(cls, `mark renders with no class — nothing can dye it: ${svg.slice(0, 60)}…`);
  return cls[1].split(/\s+/).filter(Boolean);
}

test("a mark reaches its colour through exactly one documented dye class", () => {
  // The identity channel's rule (doc/design/ui-redesign.md): a mark may only reach a colour
  // through a documented mapping, never ad hoc. There are now two such mappings and a mark
  // takes EXACTLY ONE of them — its CLI's `cli-<program>` (theme.ts §CLI_HUES) if it has one,
  // the `fleet` icon role's `ic-fleet` if it does not.
  //
  // BOTH HALVES MATTER. Wearing neither is an undyed mark, which renders in the surrounding
  // ink and looks deliberate. Wearing both would make the mark's colour depend on which of
  // the two CSS blocks happens to sit lower in styles.css — a pin that holds only by source
  // order, and test/icons.test.ts's `.ic-*` scan says in its own comment that it cannot see
  // an overriding selector, so the fight would be invisible to the suite as well.
  assert.ok(ROLE_TOKEN.fleet, "icons.ts no longer has a fleet role for undyed marks to borrow");
  const dyeOf = (svg: string) =>
    classesOf(svg).filter((c) => c.startsWith("ic-") || c.startsWith("cli-"));

  for (const program of CLI_DYE_PROGRAMS) {
    assert.deepEqual(
      dyeOf(agentMarkFor(program).svg),
      [`cli-${program}`],
      `${program} is on the CLI hue roster but does not wear its own dye class alone`
    );
  }
  // A CLI with no hue keeps the fleet violet — the colour twin of the letter badge's total
  // fallback, and an honest "loomux has no brand hue for this program".
  for (const program of ["aider", "zed-agent", "unheard-of"]) {
    assert.deepEqual(dyeOf(agentMarkFor(program).svg), ["ic-fleet"], program);
  }
  // The neutral tier must not borrow a CLI's pigment for the same reason it must not borrow
  // a CLI's caption: it is declining to name a program, and a hue would name one.
  for (const view of [agentMarkFor("ssh"), agentMark({ argv: ["ssh"], remote: true })!]) {
    assert.equal(view.kind, "unknown", "fixture drifted — this should be the neutral tier");
    assert.deepEqual(dyeOf(view.svg), ["ic-fleet"]);
  }
});

test("the dye class is a closed roster, not an interpolated program name", () => {
  // THE SECURITY PROPERTY, and it is a NEW one this feature had to answer: `program` comes
  // off a launch command (human-typed, or supplied by a workflow file) and the dye now puts
  // a program-derived token into a `class` ATTRIBUTE inside a string that pane.ts injects
  // with `innerHTML`. `class="ic cli-${program}"` would put `"><img src=x onerror=…>`
  // straight into the markup — the letter badge's clamp would still be intact and completely
  // beside the point, because the escape would be happening two attributes to the left.
  //
  // It cannot happen, and not because anything is escaped: only a name that MATCHES the
  // roster is ever interpolated, so what reaches the attribute is one of seven compile-time
  // strings. Same discipline as the clamp — make the hostile value unexpressible.
  for (const hostile of [
    `<script>alert(1)</script>`,
    `"><img src=x onerror=alert(1)>`,
    `claude" onload="alert(1)`,
    `claude x`,
    `CLAUDE`, // pre-normalization spelling: the roster is keyed the way the app spells it
    `__proto__`,
    `constructor`,
  ]) {
    const view = agentMarkFor(hostile);
    for (const c of classesOf(view.svg)) {
      assert.match(c, /^(ic|ic-fleet|cli-[a-z]+)$/, `"${hostile}" produced the class "${c}"`);
    }
    assert.equal(/\son[a-z]+\s*=/i.test(view.svg), false, `${hostile} carries an event handler`);
    assert.equal(view.svg.includes("<img"), false, hostile);
    // Exactly one `class` attribute — an injected `class="…"` of its own would also read as
    // "a class attribute" to the assertions above if they only looked at the first match.
    assert.equal((view.svg.match(/\sclass=/g) ?? []).length, 1, hostile);
  }
  // And through the two public entry points, the way a launch line actually arrives.
  assert.deepEqual(classesOf(agentMark({ command: `"><b> --go` })!.svg), ["ic", "ic-fleet"]);
  assert.deepEqual(classesOf(agentMark({ knownCli: `"><b>`, remote: true })!.svg), ["ic", "ic-fleet"]);
});

test("the CLI hue roster, the token layer and the stylesheet name the same programs", () => {
  // THREE SURFACES, ONE ROSTER, pinned in every direction — the same shape test/icons.test.ts
  // uses on the icon role table, for the same reason: each surface is only ever WRONG
  // relative to another file, so reading any one of them tells you nothing.
  //
  //   * a program in the renderer's roster with no `--cli-*` token dyes with a variable that
  //     does not exist — the mark falls back to `currentColor` and renders in header ink,
  //     which looks like a deliberate grey rather than a missing colour;
  //   * a `--cli-*` token no program claims is a pigment nothing in the app can produce;
  //   * a `.cli-*` rule with no program behind it is a hue nobody can explain, and a program
  //     with no rule is an undyed mark again.
  const fromRenderer = [...CLI_DYE_PROGRAMS].sort();

  const fromTokens = Object.keys(CSS_TOKENS)
    .filter((t) => t.startsWith("--cli-"))
    .map((t) => t.slice("--cli-".length))
    .sort();
  assert.deepEqual(fromTokens, fromRenderer, "theme.ts's --cli-* tokens and CLI_DYE_PROGRAMS drifted");

  assert.deepEqual(
    Object.keys(CLI_HUES).sort(),
    fromRenderer,
    "theme.ts's CLI_HUES and CLI_DYE_PROGRAMS drifted"
  );

  const css = read("../src/styles.css").replace(/\/\*[\s\S]*?\*\//g, "");
  const rules = new Map<string, string>();
  for (const [, program, value] of css.matchAll(/\.cli-([a-z0-9-]+)\s*\{\s*color:\s*([^;]+);/g)) {
    rules.set(program, value.trim());
  }
  assert.deepEqual([...rules.keys()].sort(), fromRenderer, "styles.css's .cli-* rules drifted");
  for (const program of fromRenderer) {
    assert.equal(
      rules.get(program),
      `var(--cli-${program})`,
      `.cli-${program} names ${rules.get(program)}, not its own token — a CLI wearing another ` +
        "CLI's pigment is the exact confusion this table exists to end"
    );
  }
});

test("every CLI on the launchable roster has a hue, or the app is still part-purple", () => {
  // The FEATURE's own claim, and the one a "we dyed the three we thought of" implementation
  // would fail. The human's note was that every agent pane came out the same violet; dyeing
  // some of `AGENTS` and not the rest fixes that for the CLIs someone remembered and leaves
  // the others in the exact state being complained about, with no signal anywhere that they
  // were skipped. Derived from src/agents.ts rather than listed here, so a CLI added to the
  // launcher arrives with this obligation attached.
  //
  // `custom` is excluded and it is the only exclusion: it names no program at all (its
  // command is whatever the human types), so `agentMarkFor` reads THAT command, and a hue
  // keyed on the literal string "custom" would dye every hand-typed CLI the same colour —
  // one violet traded for one khaki.
  const launchable = AGENTS.filter((a) => a.id !== "custom").map((a) => a.id);
  assert.ok(launchable.length >= 5, "the agent catalog shrank — this test is checking nothing");
  const undyed = launchable.filter((id) => !(CLI_DYE_PROGRAMS as readonly string[]).includes(id));
  assert.deepEqual(
    undyed,
    [],
    `these launchable CLIs still take the fleet violet: ${undyed.join(", ")} — a pane running ` +
      "one is indistinguishable from a pane running any other"
  );
  // The catalog's ids ARE the program names (src/setuppreview.ts relies on the same fact),
  // so the roster keys can be trusted to match what a launch line normalizes to.
  for (const { id, command } of AGENTS) {
    if (id === "custom") continue;
    assert.equal(command, id, `AGENTS.${id} launches "${command}" — the hue is keyed on the id`);
  }
});

test("the tooltip names the program and cannot run away", () => {
  // A letter badge is only decipherable because something spells it out on hover, so the
  // label is part of the feature rather than a nicety. It is also the one place a raw
  // program name survives, so it is length-clamped: a pathological launch line should not
  // produce a tooltip the width of the screen.
  assert.match(agentMarkFor("claude").label, /claude/);
  assert.match(agentMarkFor("copilot").label, /copilot/);
  const long = agentMarkFor("x".repeat(400));
  assert.ok(long.label.length < 64, `label is ${long.label.length} chars`);
});

test("every vendored mark's licence paperwork names it, in all three places", () => {
  // A vendored copy is only auditable if its papers point at what it actually is. Three
  // surfaces claim this pin — the module, the vendor README beside the licence text, and
  // THIRD_PARTY_NOTICES.md — and the failure mode is a re-vendor that updates one of them,
  // which is how a licence file becomes wrong rather than merely stale.
  const notices = read("../THIRD_PARTY_NOTICES.md");
  const vendorReadme = read("../src/vendor/octicons/README.md");
  for (const [where, text] of [
    ["src/vendor/octicons/README.md", vendorReadme],
    ["THIRD_PARTY_NOTICES.md", notices],
  ] as const) {
    assert.ok(text.includes(OCTICONS_PIN.commit), `${where} does not name the vendored commit`);
    assert.ok(text.includes(OCTICONS_PIN.version), `${where} does not name the vendored version`);
    for (const upstream of Object.values(MARK_SOURCES)) {
      assert.ok(text.includes(upstream), `${where} does not name the vendored glyph ${upstream}`);
    }
  }
  // The MIT grant is conditional on shipping the notice, so the text has to be there.
  assert.match(read("../src/vendor/octicons/LICENSE"), /MIT License/);
});

test("the mark's own rule adds no colour — the role class is still the only dye", () => {
  // The channel rule, checked on the surface rather than in the registry. `.ic-fleet` is
  // what makes these marks violet; if `.pane-cli-icon` grew a `color` of its own it would
  // silently win for the letter tier (whose <text> and <rect> both say `currentColor`) and
  // the mark would stop reaching its hue through a documented role — the one thing
  // doc/design/ui-redesign.md's maintainability rule 3 forbids.
  const css = read("../src/styles.css").replace(/\/\*[\s\S]*?\*\//g, "");
  const rule = css.match(/\.pane-cli-icon\s*\{([^}]*)\}/);
  assert.ok(rule, "styles.css has no .pane-cli-icon rule — the header mark is unstyled");
  assert.equal(
    /(^|;)\s*color\s*:/.test(rule[1]),
    false,
    ".pane-cli-icon paints a colour; the mark must take .ic-fleet's dye and nothing else"
  );
});

test("the label never reaches the markup — it is the accessible name, set as text", () => {
  // REVIEW N2's other half. The mark is the only thing in the header that reports which
  // CLI a pane runs, so it carries an accessible name (`role="img"` + `aria-label` on the
  // wrapper) rather than being purely decorative like the app's other icons. That name is
  // the label — the ONE value that still holds an unclamped program name — so it must be
  // set as an ATTRIBUTE by pane.ts and never interpolated into an SVG string here. If it
  // ever were, the clamp that makes this module injection-proof would be moot.
  for (const hostile of [`<img src=x onerror=alert(1)>`, `" onload="alert(1)`, `</svg><script>`]) {
    const view = agentMark({ command: hostile });
    assert.ok(view);
    assert.equal(
      view.svg.includes(view.label),
      false,
      `${hostile}: the label was interpolated into the SVG`
    );
  }
  const pane = read("../src/pane.ts");
  assert.match(pane, /setAttribute\("aria-label", view\.label\)/, "no accessible name is set");
  assert.match(pane, /setAttribute\("role", "img"\)/, "the labelled wrapper is not a role=img");
});

test("the pane header actually renders the mark", () => {
  // A resolver nothing calls is a unit test with a UI ticket attached. The DOM wiring is
  // hand-validated (this repo does not simulate a DOM), so what is checkable here is that
  // the wiring exists at all — the same shape as icons.test.ts's consumer scan.
  const pane = read("../src/pane.ts");
  assert.match(pane, /from "\.\/agenticons\.ts"/, "src/pane.ts does not import the resolver");
  assert.match(pane, /agentMark\(/, "src/pane.ts never calls agentMark");
});

/** The body of `Pane.refreshAgentMark`, on its own. Scoped to the ONE method rather than
 *  scanning all of pane.ts, so an unrelated mention of `sshDefaultCli` elsewhere in a
 *  ten-thousand-line file cannot satisfy the assertions below — a consumer scan that can be
 *  satisfied by the wrong line is a pin that reads like coverage and isn't. */
function refreshAgentMarkBody(): string {
  const pane = read("../src/pane.ts");
  const m = pane.match(/private refreshAgentMark\(\): void \{([\s\S]*?)\n {2}\}/);
  assert.ok(m, "Pane.refreshAgentMark is gone or no longer matches the expected shape");
  return m[1];
}

test("the pane feeds its SSH state to the resolver, not just its launch line", () => {
  // REVIEW NB1. The round-1 blocker (an SSH pane captioned "Agent CLI: ssh") was fixed in
  // TWO places, and only one of them was pinned: the resolver's own logic is covered by the
  // SSH test above, but nothing checked that pane.ts still HANDS it the ssh state. Deleting
  // these two lines from `refreshAgentMark` left the whole suite green — the resolver stayed
  // correct and became unreachable, which is the exact regression this round exists for and
  // the one a reader would be least likely to spot in a refactor.
  //
  // `knownCli` and `remote` are asserted separately because they answer different questions
  // and can be lost independently: without `knownCli` a remote Claude pane degrades to the
  // neutral badge (wrong but honest), while without `remote` it falls through to the launch
  // line and wears the transport again (the original defect).
  const body = refreshAgentMarkBody();
  assert.match(
    body,
    /knownCli:\s*this\.sshDefaultCli/,
    "refreshAgentMark no longer passes the SSH profile's far-end CLI — a remote agent pane " +
      "cannot name its agent, however correct the resolver is"
  );
  assert.match(
    body,
    /remote:\s*this\.isSshPane/,
    "refreshAgentMark no longer marks SSH panes as remote — the resolver will read argv[0] " +
      'and caption the pane "Agent CLI: ssh" again (#992 review B1)'
  );
});

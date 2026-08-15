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
  ICON_AGENT_PX,
  MARK_PROGRAMS,
  MARK_SOURCES,
  OCTICONS_PIN,
  agentLetter,
  agentMark,
  agentMarkFor,
} from "../src/agenticons.ts";
import { ROLE_TOKEN } from "../src/icons.ts";

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
      agentMark(command, argv as string[] | null),
      null,
      `agentMark(${JSON.stringify(command)}, ${JSON.stringify(argv)}) drew something`
    );
  }
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
    const view = agentMark(command, null);
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
  const copilot = agentMark("copilot --autopilot", null);
  assert.ok(copilot);
  assert.equal(copilot.kind, "mark");
  assert.equal(copilot.program, "copilot");

  for (const program of ["claude", "opencode"]) {
    const view = agentMark(program, null);
    assert.ok(view);
    assert.equal(
      view.kind,
      "letter",
      `${program} draws a vendored mark — which licence grants it? (module §Licensing)`
    );
  }

  // And the two letter badges are distinguishable from each other, which is the entire
  // point of drawing anything.
  assert.notEqual(agentMark("claude", null)!.svg, agentMark("opencode", null)!.svg);
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
    const view = agentMark(command, null);
    assert.ok(view, `${command} drew nothing`);
    assert.equal(view.kind, "mark", `${command} did not resolve to the copilot mark`);
  }
  // argv-only launches (a restored pane records argv, not a command string) resolve too.
  const fromArgv = agentMark(null, ["copilot", "--autopilot"]);
  assert.equal(fromArgv?.kind, "mark");

  // THE INHERITED LIMIT, pinned rather than papered over. `programFromRestore` splits the
  // command on whitespace before it looks at anything else, so a command string whose
  // program path CONTAINS a space resolves to the fragment before the space — quoted or
  // not. That is the tokenizing grammar axis (#471), which this feature deliberately does
  // not touch: it is shared with the autopilot watcher and the session reconciler, which
  // already mis-read the same line today. The mark degrades to a letter badge, which is
  // the right failure — a wrong glyph would be worse — and this assertion is here so a
  // future fix to that grammar shows up as a test to update rather than as a surprise.
  const spaced = agentMark(`C:\\Program Files\\gh\\copilot.exe --banner`, null);
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
    const view = agentMark(hostile, null);
    if (!view) continue;
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

test("the agent mark takes the fleet role's dye rather than minting a hue", () => {
  // The identity channel's rule (doc/design/ui-redesign.md): a mark may only reach a colour
  // through a documented role. "The agents themselves" is already a role — `fleet` — and an
  // agent-type glyph is the most literal possible member of it, so this module reuses that
  // class instead of adding a ninth. If someone gives these marks their own `.ic-agent`,
  // this goes red here AND in test/icons.test.ts's both-directions CSS pin, which is the
  // second half of the same argument.
  assert.ok(ROLE_TOKEN.fleet, "icons.ts no longer has a fleet role for these marks to borrow");
  for (const program of ["copilot", "claude", "unheard-of"]) {
    const cls = agentMarkFor(program).svg.match(/class="([^"]*)"/);
    assert.ok(cls, `${program} renders with no class — nothing can dye it`);
    assert.deepEqual(
      cls[1].split(/\s+/).filter((c) => c.startsWith("ic-")),
      ["ic-fleet"],
      `${program} must wear exactly the fleet role class`
    );
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

test("the pane header actually renders the mark", () => {
  // A resolver nothing calls is a unit test with a UI ticket attached. The DOM wiring is
  // hand-validated (this repo does not simulate a DOM), so what is checkable here is that
  // the wiring exists at all — the same shape as icons.test.ts's consumer scan.
  const pane = read("../src/pane.ts");
  assert.match(pane, /from "\.\/agenticons\.ts"/, "src/pane.ts does not import the resolver");
  assert.match(pane, /agentMark\(/, "src/pane.ts never calls agentMark");
});

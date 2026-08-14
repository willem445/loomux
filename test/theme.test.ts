// The colour seam — theme.ts and the three surfaces pinned to it (#879, slice A).
//
// loomux paints in three languages that cannot read each other. The stylesheet's `:root`
// custom properties style the chrome. The `<style>` block in index.html paints the app
// ground BEFORE the bundle exists, because otherwise startup flashes an unstyled white
// page. And xterm.js takes an ITheme object, because terminals render on a WebGL canvas
// where CSS custom properties do not reach. Three copies of the same decision, in CSS, in
// HTML, and in TypeScript, with nothing but care keeping them equal.
//
// src/theme.ts is now the one copy, and these tests are what make that true rather than
// aspirational: each surface is read from disk and compared against the module. A palette
// edit that lands in two of the three places goes red here instead of shipping as a
// one-frame flash of the previous release's background, or as a terminal whose colours
// belong to a design nobody kept.
//
// The ANSI test earns its place separately: sixteen slots of near-identical hex strings is
// exactly the shape a copy-paste typo hides in. Collapse two slots and every CLI that uses
// the losing one goes invisible — no error, no exception, and no other test in this repo
// would notice. Run `npm test`.
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import {
  ANSI_SLOTS,
  CSS_TOKENS,
  IDENTITY,
  IDENTITY_LIT,
  PALETTE,
  PRE_PAINT_BACKGROUND,
  SEMANTIC,
  TERMINAL_THEME,
} from "../src/theme.ts";

const read = (rel: string) => readFileSync(new URL(rel, import.meta.url), "utf8");
const stripCssComments = (s: string) => s.replace(/\/\*[\s\S]*?\*\//g, "");
const HEX = /^#[0-9a-f]{6}$/;

// WCAG relative luminance / contrast. The design note (doc/design/ui-redesign.md) makes
// contrast PROMISES about this palette; a promise nobody measures is prose.
function luminance(hex: string): number {
  const n = Number.parseInt(hex.slice(1), 16);
  const channel = (c: number) => {
    const s = c / 255;
    return s <= 0.03928 ? s / 12.92 : ((s + 0.055) / 1.055) ** 2.4;
  };
  return (
    0.2126 * channel((n >> 16) & 255) +
    0.7152 * channel((n >> 8) & 255) +
    0.0722 * channel(n & 255)
  );
}
function contrast(a: string, b: string): number {
  const [hi, lo] = [luminance(a), luminance(b)].sort((x, y) => y - x);
  return (hi + 0.05) / (lo + 0.05);
}

/**
 * The identity channel's hues, base and Lit, under distinct names.
 *
 * NOT `{ ...IDENTITY, ...IDENTITY_LIT }` — those two maps share their key names by design,
 * so spreading them silently drops all eight base hues and leaves only the Lit steps. An
 * earlier version of these tests did exactly that and consequently never checked a base hue
 * for anything; it was caught by mutating `orchid` to violet's hex and watching the
 * distinctness assertion stay green.
 */
function identityEntries(): [string, string][] {
  return (Object.keys(IDENTITY) as (keyof typeof IDENTITY)[]).flatMap(
    (name): [string, string][] => [
      [name, IDENTITY[name]],
      [`${name}Lit`, IDENTITY_LIT[name]],
    ]
  );
}

// --- perceptual distance, and what colour-vision deficiency does to it.
//
// The design note promises that the STATE channel survives colour blindness and that the
// IDENTITY channel is allowed not to. That is a measurable claim about eight hex values,
// so it is measured here. CIE76 in Lab is coarse but monotone enough to rank "can these
// two be told apart", which is the only question being asked.
function linearRgb(hex: string): [number, number, number] {
  const n = Number.parseInt(hex.slice(1), 16);
  const ch = (c: number) => {
    const s = c / 255;
    return s <= 0.04045 ? s / 12.92 : ((s + 0.055) / 1.055) ** 2.4;
  };
  return [ch((n >> 16) & 255), ch((n >> 8) & 255), ch(n & 255)];
}
function lab(hex: string): [number, number, number] {
  const [r, g, b] = linearRgb(hex);
  const x = (0.4124 * r + 0.3576 * g + 0.1805 * b) / 0.95047;
  const y = 0.2126 * r + 0.7152 * g + 0.0722 * b;
  const z = (0.0193 * r + 0.1192 * g + 0.9505 * b) / 1.08883;
  const f = (t: number) => (t > 0.008856 ? Math.cbrt(t) : 7.787 * t + 16 / 116);
  return [116 * f(y) - 16, 500 * (f(x) - f(y)), 200 * (f(y) - f(z))];
}
function deltaE(a: string, b: string): number {
  const [l1, a1, b1] = lab(a);
  const [l2, a2, b2] = lab(b);
  return Math.hypot(l1 - l2, a1 - a2, b1 - b2);
}

/** Dichromat simulation in LMS space (the standard Viénot/Brettel matrices). */
type Cvd = "protan" | "deutan" | "tritan";
const CVD_KINDS: readonly Cvd[] = ["protan", "deutan", "tritan"];
function simulate(hex: string, kind: Cvd): string {
  const [r, g, b] = linearRgb(hex);
  const l = 17.8824 * r + 43.5161 * g + 4.11935 * b;
  const m = 3.45565 * r + 27.1554 * g + 3.86714 * b;
  const s = 0.0299566 * r + 0.184309 * g + 1.46709 * b;
  const l2 = kind === "protan" ? 2.02344 * m - 2.52581 * s : l;
  const m2 = kind === "deutan" ? 0.494207 * l + 1.24827 * s : m;
  const s2 = kind === "tritan" ? -0.395913 * l + 0.801109 * m : s;
  const out: [number, number, number] = [
    0.080944 * l2 - 0.130504 * m2 + 0.116721 * s2,
    -0.0102485 * l2 + 0.0540194 * m2 - 0.113615 * s2,
    -0.000365294 * l2 - 0.00412163 * m2 + 0.693513 * s2,
  ];
  const enc = (v: number) => {
    const c = Math.min(1, Math.max(0, v));
    const srgb = c <= 0.0031308 ? 12.92 * c : 1.055 * c ** (1 / 2.4) - 0.055;
    return Math.round(255 * srgb)
      .toString(16)
      .padStart(2, "0");
  };
  return `#${out.map(enc).join("")}`;
}

/** The closest pair in a set, after `view` is applied to every member. */
function closestPair(
  set: Record<string, string>,
  view: (hex: string) => string
): { distance: number; a: string; b: string } {
  const names = Object.keys(set);
  let best = { distance: Number.POSITIVE_INFINITY, a: "", b: "" };
  for (let i = 0; i < names.length; i++) {
    for (let j = i + 1; j < names.length; j++) {
      const d = deltaE(view(set[names[i]]), view(set[names[j]]));
      if (d < best.distance) best = { distance: d, a: names[i], b: names[j] };
    }
  }
  return best;
}

test("every ANSI slot is present, and no two slots share a colour", () => {
  const seen = new Map<string, string>();
  for (const slot of ANSI_SLOTS) {
    const value: string = TERMINAL_THEME[slot];
    assert.ok(value !== undefined, `ANSI slot ${slot} is missing from TERMINAL_THEME`);
    assert.match(value, HEX, `ANSI slot ${slot} is not a 6-digit hex colour: ${value}`);
    const clash = seen.get(value);
    assert.equal(
      clash,
      undefined,
      `ANSI slots ${clash} and ${slot} are both ${value} — one of them is invisible`
    );
    seen.set(value, slot);
  }
  assert.equal(seen.size, 16, "the terminal needs all sixteen ANSI colours");
});

test("no ANSI colour disappears into the terminal background", () => {
  for (const slot of ANSI_SLOTS) {
    const value: string = TERMINAL_THEME[slot];
    assert.notEqual(
      value,
      TERMINAL_THEME.background,
      `ANSI ${slot} is the terminal background — text in it would be unreadable`
    );
    // ANSI black is meant to be dim, not absent; everything else is meant to be read.
    const floor = slot === "black" ? 1.2 : 3;
    assert.ok(
      contrast(value, TERMINAL_THEME.background) >= floor,
      `ANSI ${slot} (${value}) is ${contrast(value, TERMINAL_THEME.background).toFixed(2)}:1 ` +
        `on the terminal background — below the ${floor}:1 floor`
    );
  }
});

test("the stylesheet declares every pinned token, with theme.ts's value", () => {
  const css = stripCssComments(read("../src/styles.css"));
  const root = css.match(/:root\s*\{([\s\S]*?)\}/);
  assert.ok(root, "styles.css has no :root block — the token layer is the first thing in it");
  const declared = new Map<string, string>();
  for (const [, name, value] of root[1].matchAll(/(--[a-z0-9-]+)\s*:\s*([^;]+);/g)) {
    declared.set(name, value.trim());
  }
  for (const [name, expected] of Object.entries(CSS_TOKENS)) {
    assert.equal(
      declared.get(name),
      expected,
      `styles.css ${name} is ${declared.get(name) ?? "undeclared"}, theme.ts says ${expected}`
    );
  }
});

// The pin above runs theme.ts -> stylesheet. On its own that is one-directional: a token
// minted straight into `:root` with a literal value is a FOURTH copy of a colour, and it
// stays green because nothing walks the stylesheet back into CSS_TOKENS. Slice B mints
// tokens by the dozen while migrating ~387 literals, which is exactly when that hole gets
// used. So walk it the other way too: every raw colour declared in `:root` must be pinned.
//
// A `var(...)` value is not a raw colour — it is an alias onto something already pinned,
// which is what the legacy bridge is made of, so the bridge passes this without exception.
const BRIDGE_LITERALS = new Set([
  // The one bridge declaration that is a literal rather than an alias: an alpha companion
  // to --accent, which CSS cannot derive from a hex custom property without color-mix.
  // It dies with the bridge in slice B, and no new entry may be added to this set.
  "--accent-glow",
]);
const RAW_COLOUR = /^(#|(rgba?|hsla?|hwb|lab|lch|oklab|oklch|color)\()/i;

test("no colour enters :root without a pin in theme.ts", () => {
  const css = stripCssComments(read("../src/styles.css"));
  const root = css.match(/:root\s*\{([\s\S]*?)\n\}/);
  assert.ok(root, "styles.css has no :root block");
  const unpinned: string[] = [];
  for (const [, name, value] of root[1].matchAll(/(--[a-z0-9-]+)\s*:\s*([^;]+);/g)) {
    const v = value.trim();
    if (!RAW_COLOUR.test(v)) continue;
    if (name in CSS_TOKENS || BRIDGE_LITERALS.has(name)) continue;
    unpinned.push(`${name}: ${v}`);
  }
  assert.deepEqual(
    unpinned,
    [],
    "these :root colours exist nowhere in theme.ts, so nothing keeps them equal to the " +
      `pre-paint block or the terminal: ${unpinned.join("; ")}`
  );
});

test("index.html paints theme.ts's app ground before the bundle arrives", () => {
  const html = read("../index.html").replace(/<!--[\s\S]*?-->/g, "");
  const style = html.match(/<style>([\s\S]*?)<\/style>/);
  assert.ok(style, "index.html has no critical <style> block — startup would flash white");
  const backgrounds = [...stripCssComments(style[1]).matchAll(/background:\s*([^;]+);/g)].map(
    (m) => m[1].trim()
  );
  assert.deepEqual(
    backgrounds,
    [PRE_PAINT_BACKGROUND],
    "the pre-paint background must be exactly theme.ts's PRE_PAINT_BACKGROUND"
  );
});

test("pane.ts carries no colour of its own", () => {
  const src = read("../src/pane.ts")
    .replace(/\/\*[\s\S]*?\*\//g, "")
    .replace(/^\s*\/\/.*$/gm, "");
  const hexes = [...src.matchAll(/#[0-9a-fA-F]{6}\b/g)].map((m) => m[0]);
  assert.deepEqual(
    hexes,
    [],
    `pane.ts declares colours directly (${hexes.join(", ")}) — they belong in theme.ts, ` +
      "where the stylesheet and the pre-paint block can be pinned to them"
  );
  assert.match(
    src,
    /import\s*\{[^}]*TERMINAL_THEME[^}]*\}\s*from\s*"\.\/theme(\.ts)?"/,
    "pane.ts must take its xterm ITheme from theme.ts"
  );
});

test("the ink ramp keeps the contrast the design note promises", () => {
  const grounds = [SEMANTIC.surfaceTerm, SEMANTIC.surface0, SEMANTIC.surface1, SEMANTIC.surface2];
  for (const ground of grounds) {
    assert.ok(
      contrast(SEMANTIC.ink, ground) >= 7,
      `ink on ${ground} is ${contrast(SEMANTIC.ink, ground).toFixed(2)}:1, below AAA (7:1)`
    );
    assert.ok(
      contrast(SEMANTIC.inkDim, ground) >= 4.5,
      `dim ink on ${ground} is ${contrast(SEMANTIC.inkDim, ground).toFixed(2)}:1, below AA`
    );
    // Faint ink is deliberately below AA: the design note restricts it to non-essential
    // meta and rules. If it ever clears AA it has stopped being a separate role — and if
    // it drops below 3:1 it is invisible. Both are corrections, not passes.
    const faint = contrast(SEMANTIC.inkFaint, ground);
    assert.ok(faint >= 3 && faint < 4.5, `faint ink on ${ground} is ${faint.toFixed(2)}:1`);
  }
});

test("every state dye is readable on every surface, and no two states share one", () => {
  const states = {
    working: SEMANTIC.stateWorking,
    attention: SEMANTIC.stateAttention,
    ok: SEMANTIC.stateOk,
    danger: SEMANTIC.stateDanger,
    held: SEMANTIC.stateHeld,
    idle: SEMANTIC.stateIdle,
  };
  assert.equal(
    new Set(Object.values(states)).size,
    Object.keys(states).length,
    "two agent states are painted the same colour — the fleet view stops telling them apart"
  );
  // `held` and `idle` are achromatic by design (form, not hue, marks a stopped agent), so
  // the readability floor applies to the four dyes that a user must recognise at a glance.
  for (const name of ["working", "attention", "ok", "danger"] as const) {
    for (const ground of [SEMANTIC.surface0, SEMANTIC.surface1, SEMANTIC.surface2]) {
      const ratio = contrast(states[name], ground);
      assert.ok(ratio >= 4.5, `${name} on ${ground} is ${ratio.toFixed(2)}:1, below AA`);
    }
  }
});

test("the surface ramp climbs, quietly", () => {
  const ramp = [SEMANTIC.surfaceTerm, SEMANTIC.surface0, SEMANTIC.surface1, SEMANTIC.surface2];
  for (let i = 1; i < ramp.length; i++) {
    const step = contrast(ramp[i], ramp[i - 1]);
    assert.ok(
      luminance(ramp[i]) > luminance(ramp[i - 1]),
      `surface ${i} is not lighter than surface ${i - 1} — the depth order is inverted`
    );
    // The cockpit look depends on panels sitting CLOSE: separation comes from a hairline
    // and spacing, not from a contrast block. A step that grows past ~1.3:1 is the
    // heavy-panel look this design rejected.
    assert.ok(step < 1.3, `surface step ${i - 1}->${i} is ${step.toFixed(2)}:1 — too loud`);
  }
});

test("every identity hue is readable as text on every surface it can sit on", () => {
  // The identity channel puts hue on icons, lanes, tabs, meters and chips — surfaces that
  // carry LABELS, not just marks. So the floor is AA (4.5:1) at text size on the three app
  // grounds, not the 3:1 non-text floor: a hue that only cleared 3:1 would force every
  // consumer to reason about whether its use was "text enough", and slice B through M would
  // each answer differently.
  for (const [name, value] of Object.entries(IDENTITY)) {
    for (const ground of [SEMANTIC.surface0, SEMANTIC.surface1, SEMANTIC.surface2]) {
      const ratio = contrast(value, ground);
      assert.ok(
        ratio >= 4.5,
        `identity ${name} (${value}) on ${ground} is ${ratio.toFixed(2)}:1, below AA`
      );
    }
  }
  // WCAG 1.4.11: a non-text mark — an icon stroke, a lane, a meter fill — needs 3:1. The
  // terminal ground is included here and not above, because an identity mark can sit over a
  // terminal (a pane badge) where a label never does.
  for (const [name, value] of identityEntries()) {
    const ratio = contrast(value, SEMANTIC.surfaceTerm);
    assert.ok(
      ratio >= 3,
      `identity ${name} (${value}) on the terminal ground is ${ratio.toFixed(2)}:1, ` +
        "below the WCAG 1.4.11 non-text floor"
    );
  }
});

test("the identity channel is eight distinct hues, each with its Lit step", () => {
  assert.deepEqual(
    Object.keys(IDENTITY),
    Object.keys(IDENTITY_LIT),
    "every identity hue needs its Lit companion, in the same order"
  );
  assert.ok(
    Object.keys(IDENTITY).length >= 8 && Object.keys(IDENTITY).length <= 10,
    `the identity channel holds ${Object.keys(IDENTITY).length} hues; the brief argues for 8-10 ` +
      "— fewer reads as the near-monochrome look the direction gate rejected, more is fruit salad"
  );
  const entries = identityEntries();
  const byValue = new Map<string, string>();
  for (const [name, value] of entries) {
    const clash = byValue.get(value);
    assert.equal(
      clash,
      undefined,
      `identity tokens "${clash}" and "${name}" are both ${value} — one of them cannot say ` +
        "which thing it is"
    );
    byValue.set(value, name);
  }
  assert.equal(byValue.size, entries.length, "the identity channel lost a distinct value");
  // Every Lit step must actually be lighter than its base, or "Lit" is a lie a consumer
  // reaching for emphasis would silently get wrong.
  for (const name of Object.keys(IDENTITY) as (keyof typeof IDENTITY)[]) {
    assert.ok(
      luminance(IDENTITY_LIT[name]) > luminance(IDENTITY[name]),
      `${name}Lit is not lighter than ${name} — the emphasis step goes the wrong way`
    );
  }
});

const STATE_DYES = {
  working: SEMANTIC.stateWorking,
  attention: SEMANTIC.stateAttention,
  ok: SEMANTIC.stateOk,
  danger: SEMANTIC.stateDanger,
};

test("the four agent states stay separable under colour-vision deficiency", () => {
  // THE LOAD-BEARING MEASUREMENT OF THE THREE-CHANNEL DESIGN.
  //
  // Eight hues on one dark ground cannot all survive CVD, and this set does not: azure and
  // violet differ by 2.9 dE to a protanope, rose and orchid are identical to a tritanope.
  // The design accepts that for IDENTITY — which thing this is, always also carried by
  // position, label and icon shape — and refuses it for STATE, which is the one thing a
  // supervisor has to read correctly at a glance across ten panes.
  //
  // Measured worst case for the shipped dyes is 10.3 dE (tritan, attention/danger, where
  // amber and rose both lose their yellow axis). The floor is 9: low enough that a
  // legitimate nudge to a dye does not trip it, high enough that two states merging does.
  for (const kind of [null, ...CVD_KINDS]) {
    const view = (hex: string) => (kind === null ? hex : simulate(hex, kind));
    const { distance, a, b } = closestPair(STATE_DYES, view);
    assert.ok(
      distance >= 9,
      `${kind ?? "normal vision"}: agent states "${a}" and "${b}" are ${distance.toFixed(1)} dE ` +
        "apart — a supervisor cannot tell those two panes apart"
    );
  }
});

test("no identity-only hue may fill a state role", () => {
  // The channel rule, enforced where a token could actually break it.
  //
  // Four of the eight hues carry a state role as well as an identity one; the other four —
  // lime, cyan, violet, orchid — are identity ONLY. Promoting one into a state position is
  // the specific regression the three-channel design fears, and it is not something a
  // contrast or a distance check can catch: `stateOk = cyan` measures perfectly fine and is
  // still wrong, because it spends a hue the identity channel was relying on and puts the
  // fleet's readability on a channel that collapses under CVD.
  // The four names are written out HERE, as the design's own claim, and deliberately NOT
  // derived by subtracting the state dyes from IDENTITY. A derived set redefines itself the
  // moment the thing it is meant to catch happens: promote lime into `stateOk` and lime is
  // no longer "identity-only", so a subtractive check exonerates the very edit it exists to
  // refuse. That was the first version of this test, and mutating `stateOk := PALETTE.lime`
  // left it green.
  const IDENTITY_ONLY = ["lime", "cyan", "violet", "orchid"] as const;

  const byHex = new Map<string, string>();
  for (const name of IDENTITY_ONLY) {
    const hex: string | undefined = IDENTITY[name];
    assert.ok(
      hex !== undefined,
      `the identity channel has lost "${name}" — either it was renamed, in which case fix ` +
        "this list, or the channel is shrinking back toward the near-monochrome palette"
    );
    byHex.set(hex, name);
  }
  // The list must also still be identity-ONLY: if one of these ever became a state dye by
  // some other route, the whole premise above is void.
  for (const [role, hex] of Object.entries(STATE_DYES)) {
    assert.equal(
      byHex.get(hex),
      undefined,
      `state role "${role}" is painted ${hex}, which is the identity-only hue ` +
        `"${byHex.get(hex)}" — an identity hue may never sit in a state position ` +
        "(design note, §The three colour channels)"
    );
  }
  for (const [role, hex] of [
    ["held", SEMANTIC.stateHeld],
    ["idle", SEMANTIC.stateIdle],
  ] as const) {
    assert.equal(
      byHex.get(hex),
      undefined,
      `state role "${role}" is painted ${hex}, an identity hue — held and idle are achromatic ` +
        "by design: a stopped agent is marked by form, not by hue"
    );
  }
});

test("every palette entry is a well-formed hex colour", () => {
  for (const [name, value] of Object.entries(PALETTE)) {
    assert.match(value, HEX, `PALETTE.${name} is not a 6-digit lowercase hex: ${value}`);
  }
});

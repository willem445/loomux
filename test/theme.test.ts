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

test("every palette entry is a well-formed hex colour", () => {
  for (const [name, value] of Object.entries(PALETTE)) {
    assert.match(value, HEX, `PALETTE.${name} is not a 6-digit lowercase hex: ${value}`);
  }
});

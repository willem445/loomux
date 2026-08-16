// The icon registry and its role→dye table (#879, slice K).
//
// The human's ask was "really nice icons of different colours", and the design note turns
// that into something checkable: colour is the IDENTITY channel, an icon's hue says WHICH
// thing it is, and it may only reach a hue through a documented role mapping
// (doc/design/ui-redesign.md, §The three colour channels, maintainability rule 3). Prose
// like that survives exactly as long as the next person who edits the table, so the claims
// are measured here instead:
//
//   * the artwork carries no colour at all — recolouring the app must never touch an SVG;
//   * each of the eight identity hues is claimed by EXACTLY ONE role, so a colour on screen
//     resolves to one meaning rather than to whichever family happens to be amber;
//   * no role may name a `--state-*` token, which is the specific regression that would put
//     fleet legibility on the channel the brief measured as collapsing under colour-vision
//     deficiency;
//   * the stylesheet and the registry agree in BOTH directions — a role with no CSS rule
//     renders in plain ink and looks like a design decision nobody made;
//   * nothing is vendored that no surface renders, because an unused vendored glyph is a
//     licence obligation with no benefit;
//   * the provenance pin says the same commit in all three places that claim it.
//
// Run `npm test`.
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync, readdirSync } from "node:fs";
import {
  ICON_NAMES,
  ICON_ROLE,
  ICON_VIEWBOX,
  LUCIDE_PIN,
  ROLE_TOKEN,
  icon,
  type IconName,
  type IconRole,
} from "../src/icons.ts";
import { CATEGORY_ICON, iconSvg, type IconCategory } from "../src/fileicons.ts";
import { CSS_TOKENS } from "../src/theme.ts";

const read = (rel: string) => readFileSync(new URL(rel, import.meta.url), "utf8");
const stripCssComments = (s: string) => s.replace(/\/\*[\s\S]*?\*\//g, "");

/** The markup between the wrapper's `<svg …>` and `</svg>` — i.e. the vendored artwork,
 *  with nothing this module added. */
function body(name: IconName): string {
  const m = icon(name).match(/^<svg\b[^>]*>([\s\S]*)<\/svg>$/);
  assert.ok(m, `icon("${name}") is not a single well-formed <svg> element`);
  return m[1];
}

const ROLES = Object.keys(ROLE_TOKEN) as IconRole[];

test("no vendored glyph carries a colour of its own", () => {
  // THE LOAD-BEARING PROPERTY OF THE WHOLE SLICE. If a body held a literal, that icon would
  // be the one thing in the app the token layer cannot reach: it would keep its colour
  // through a palette change, through the identity channel, through everything — and it
  // would do so silently, because a hard-coded hex renders perfectly well.
  //
  // `currentColor` is deliberately allowed as a `fill`, and that is not a loophole: Lucide's
  // `palette` glyph fills its four dots that way, which is a colour REFERENCE — it resolves
  // to whatever the role class set — not a colour. A test that banned `fill=` outright would
  // have rejected the artwork; one that banned only hexes would have missed `fill="red"`.
  const LITERAL = /#[0-9a-fA-F]{3,8}\b|\b(rgba?|hsla?|hwb|lab|lch|oklab|oklch)\s*\(/;
  for (const name of ICON_NAMES) {
    const b = body(name);
    assert.equal(
      LITERAL.test(b),
      false,
      `${name}'s artwork contains a colour literal — the palette can no longer reach it`
    );
    for (const [, attr, value] of b.matchAll(/\b(fill|stroke|stop-color|color)\s*=\s*"([^"]*)"/g)) {
      assert.ok(
        value === "currentColor" || value === "none",
        `${name} paints ${attr}="${value}"; a vendored body may only say currentColor or none`
      );
    }
  }
});

test("every icon renders on one grid, in currentColor, wearing exactly one role class", () => {
  // The old hand-drawn set was four private conventions (a 16 grid at four stroke weights),
  // which is why it read as a pile of marks rather than a set. Uniformity is the thing that
  // makes a vendored set worth having, so it is asserted rather than assumed.
  for (const name of ICON_NAMES) {
    const svg = icon(name);
    assert.ok(svg.startsWith("<svg ") && svg.endsWith("</svg>"), `${name} is not one element`);
    assert.ok(
      svg.includes(`viewBox="${ICON_VIEWBOX}"`),
      `${name} is not on the registry's grid — it will sit at a different optical weight`
    );
    assert.ok(svg.includes(`stroke="currentColor"`), `${name} does not inherit its colour`);

    const cls = svg.match(/class="([^"]*)"/);
    assert.ok(cls, `${name} renders with no class — nothing can dye it`);
    const roleClasses = cls[1].split(/\s+/).filter((c) => c.startsWith("ic-"));
    assert.deepEqual(
      roleClasses,
      [`ic-${ICON_ROLE[name]}`],
      `${name} must carry exactly one role class, the one its registry entry declares`
    );
  }
});

test("every glyph declares a role, and every role is a real one", () => {
  for (const name of ICON_NAMES) {
    const role = ICON_ROLE[name];
    assert.ok(role !== undefined, `${name} is vendored but declares no role, so it has no dye`);
    assert.ok(ROLES.includes(role), `${name} declares role "${role}", which has no token`);
  }
  // And no role entry for a glyph that is not in the registry — a stale row here is a
  // meaning assigned to nothing.
  for (const name of Object.keys(ICON_ROLE)) {
    assert.ok(
      (ICON_NAMES as string[]).includes(name),
      `the role table names "${name}", which is not a vendored glyph`
    );
  }
});

test("each identity hue is claimed by exactly one role", () => {
  // THE BIJECTION, and it is what keeps eight hues from becoming decoration. If two roles
  // shared a hue, that hue would stop answering "which thing is this" — the user would see
  // amber and have to work out whether it meant source or tasks — and the table would have
  // silently become a palette of nice colours instead of a legend.
  //
  // It also fixes the ceiling: the brief measured eight as the honest number of hues, so a
  // ninth icon family cannot be smuggled in by minting a colour. It has to displace one.
  const identityTokens = Object.keys(CSS_TOKENS).filter((t) => t.startsWith("--id-"));
  const claimed = new Map<string, IconRole>();
  for (const role of ROLES) {
    const token = ROLE_TOKEN[role];
    assert.ok(
      identityTokens.includes(token),
      `role "${role}" dyes with ${token}, which the token layer does not declare`
    );
    const rival = claimed.get(token);
    assert.equal(
      rival,
      undefined,
      `roles "${rival}" and "${role}" both dye with ${token} — that hue now means two things`
    );
    claimed.set(token, role);
  }
  assert.deepEqual(
    identityTokens.filter((t) => !claimed.has(t)),
    [],
    "an identity hue is declared but no icon role claims it — either give it a meaning or " +
      "the palette is carrying a colour nothing can explain"
  );
});

test("no icon role may dye with a state or interaction token", () => {
  // The channel rule, enforced where an edit could actually break it. `--state-working` and
  // `--id-azure` are the SAME PIGMENT by design, so this cannot be a check on hexes — it is
  // a check on the token a role NAMES, which is exactly how the brief says a surface
  // declares which question it is answering.
  //
  // Why it matters more here than anywhere else: the four state dyes are the one set that
  // survives colour-vision deficiency, and they survive it because nothing outside a state
  // POSITION spends them. An icon dyed `--state-danger` would look right, measure right, and
  // still have moved a fleet signal into the channel that collapses.
  const forbidden = Object.keys(CSS_TOKENS).filter(
    (t) => t.startsWith("--state-") || t === "--accent" || t === "--focus" || t === "--selection"
  );
  for (const role of ROLES) {
    const token = ROLE_TOKEN[role];
    assert.equal(
      forbidden.includes(token),
      false,
      `role "${role}" dyes with ${token} — an icon reports which thing this is, never what ` +
        "state it is in; state marks take their colour from the position they sit in"
    );
    assert.ok(
      token.startsWith("--id-"),
      `role "${role}" dyes with ${token}, which is not an identity token`
    );
  }
});

test("the stylesheet dyes every role, with the registry's token, and dyes nothing else", () => {
  // The pin, both directions — the same shape test/theme.test.ts uses on `:root`, for the
  // same reason. Registry → CSS catches a role added in TypeScript with no rule: it renders
  // in the surrounding ink, which looks deliberate and is not. CSS → registry catches a
  // `.ic-*` rule left behind by a role that was renamed or dropped: a hue in the stylesheet
  // that nothing in the app can produce, and no way to tell by reading either file alone.
  //
  // It is a textual scan and enumerates its own limit: it only sees the bare `.ic-<role> {
  // color: … }` rule, so a later, more specific selector (`.pane.active .ic-vcs { color: … }`)
  // would win the cascade and override the pin without this test ever seeing it. None exists
  // today — don't be the first.
  //
  // The `.cli-*` block beside this one in styles.css (the per-CLI dyes, theme.ts §CLI_HUES)
  // is deliberately NOT such a selector: src/agenticons.ts stamps `cli-<program>` OR
  // `ic-fleet` and never both, so the two tables never meet on one element and neither has to
  // out-specify the other. That either/or is what keeps this scan honest, and
  // test/agenticons.test.ts is where it is pinned.
  const css = stripCssComments(read("../src/styles.css"));
  const declared = new Map<string, string>();
  for (const [, role, value] of css.matchAll(/\.ic-([a-z0-9-]+)\s*\{\s*color:\s*([^;]+);/g)) {
    declared.set(role, value.trim());
  }
  for (const role of ROLES) {
    assert.equal(
      declared.get(role),
      `var(${ROLE_TOKEN[role]})`,
      `styles.css dyes .ic-${role} with ${declared.get(role) ?? "nothing"}, the registry says ` +
        `var(${ROLE_TOKEN[role]})`
    );
  }
  assert.deepEqual(
    [...declared.keys()].filter((r) => !(ROLES as string[]).includes(r)),
    [],
    "styles.css dyes an icon role the registry has never heard of"
  );
});

test("nothing is vendored that no surface renders", () => {
  // "Bundle only the icons you use" is the whole argument for vendoring rather than taking a
  // dependency: the copy stays small enough to audit. Nothing enforces that by itself —
  // adding a glyph "while I'm in here" is free and invisible — so the set is checked against
  // the consumers. A vendored glyph nobody renders is a licence obligation buying nothing.
  //
  // A bare substring search is too loose: a generic name like "file" or "folder" turns up in
  // comments, type names and unrelated strings across these files whether or not anything
  // actually renders that glyph — so the two real call shapes are matched instead of the raw
  // text. A direct `icon("<name>", …)` call is one; the other is a value in `CATEGORY_ICON`
  // (fileicons.ts), the only place a call site names a glyph indirectly (`icon(CATEGORY_ICON
  // [category], …)`).
  const dir = new URL("../src/", import.meta.url);
  const consumers = readdirSync(dir)
    .filter((f) => f.endsWith(".ts") && f !== "icons.ts")
    .map((f) => readFileSync(new URL(f, dir), "utf8"))
    .join("\n");
  const used = new Set<string>();
  for (const [, name] of consumers.matchAll(/\bicon\(\s*"([a-z0-9-]+)"/g)) used.add(name);
  const categoryIcon = consumers.match(/CATEGORY_ICON[^=]*=\s*\{([\s\S]*?)\}/);
  assert.ok(categoryIcon, "CATEGORY_ICON's own definition moved or was renamed");
  for (const [, name] of categoryIcon[1].matchAll(/:\s*"([a-z0-9-]+)"/g)) used.add(name);
  const unused = ICON_NAMES.filter((n) => !used.has(n));
  assert.deepEqual(
    unused,
    [],
    `these glyphs are vendored but no surface asks for them: ${unused.join(", ")}`
  );
});

test("the Lucide provenance pin says the same thing in all three places", () => {
  // A vendored copy is only auditable if its papers point at the version it actually is.
  // Three surfaces claim this commit — the module, the vendor README beside the licence, and
  // THIRD_PARTY_NOTICES.md — and a re-vendor that updates one is the ordinary way a licence
  // file becomes wrong rather than merely stale (CLAUDE.md: correcting a claim is a
  // multi-surface edit).
  for (const [where, text] of [
    ["src/vendor/lucide/README.md", read("../src/vendor/lucide/README.md")],
    ["THIRD_PARTY_NOTICES.md", read("../THIRD_PARTY_NOTICES.md")],
  ] as const) {
    assert.ok(
      text.includes(LUCIDE_PIN.commit),
      `${where} does not name the commit src/icons.ts is vendored from (${LUCIDE_PIN.commit})`
    );
    assert.ok(
      text.includes(LUCIDE_PIN.version),
      `${where} does not name the vendored Lucide version (${LUCIDE_PIN.version})`
    );
  }
  // The licence text itself has to be there — the ISC grant is conditional on shipping it.
  assert.match(read("../src/vendor/lucide/LICENSE"), /ISC License/);
});

test("an icon renders in the box its call site asks for", () => {
  // The migration's promise to slices C-I: this slice colours icons, it does not move
  // anything. Every migrated call site passes the box its hand-drawn glyph already had.
  assert.ok(icon("folder", 12).includes(`width="12" height="12"`));
  assert.ok(icon("folder", 13).includes(`width="13" height="13"`));
  assert.ok(icon("folder").includes(`width="14" height="14"`), "the default box is the tree's");
});

test("every file category resolves to a distinct vendored glyph", () => {
  // The file tree is the densest icon surface in the app, and the one where the colour is
  // most of the point: three hues group the kinds (folders, code, content) while fifteen
  // shapes separate the members. Two categories sharing a glyph would silently merge two
  // kinds of file into one row type, which is the failure this listing exists to prevent.
  const seen = new Map<string, IconCategory>();
  for (const [category, name] of Object.entries(CATEGORY_ICON) as [IconCategory, IconName][]) {
    assert.ok(
      (ICON_NAMES as string[]).includes(name),
      `category "${category}" wants glyph "${name}", which is not vendored`
    );
    const rival = seen.get(name);
    assert.equal(rival, undefined, `categories "${rival}" and "${category}" draw the same glyph`);
    seen.set(name, category);
  }
  // And the rendered strings differ, which is what a reader of the tree actually sees.
  const svgs = new Set(Object.keys(CATEGORY_ICON).map((c) => iconSvg(c as IconCategory)));
  assert.equal(svgs.size, Object.keys(CATEGORY_ICON).length);
});

test("an icon string is safe to hand to innerHTML", () => {
  // Every consumer injects these with `innerHTML`, which is fine for line art and would not
  // be fine for anything else. The bodies are vendored from an upstream repo, so "we copied
  // it carefully" is the only thing standing between a bad paste and script execution in a
  // Tauri webview that can reach the IPC bridge. Cheap to check, so check it.
  for (const name of ICON_NAMES) {
    const svg = icon(name);
    assert.equal(/<script|<foreignObject|javascript:/i.test(svg), false, `${name} is not line art`);
    assert.equal(/\son[a-z]+\s*=/i.test(svg), false, `${name} carries an event handler`);
    assert.equal(/\b(href|xlink:href|src)\s*=/i.test(svg), false, `${name} references something`);
  }
});

// The loomux palette — the single source of truth for every colour loomux paints (#879).
//
// WHY THIS MODULE EXISTS. loomux's colours live in three languages that cannot see each
// other: the `:root` custom properties in styles.css, the critical pre-paint `background`
// in index.html (which must be right BEFORE the bundle loads, or the app flashes the wrong
// colour at startup), and the xterm.js ITheme in pane.ts (terminals render on a WebGL
// canvas — CSS custom properties are invisible to them). Before this module the three were
// hand-kept in sync, which is to say they were not: the pre-paint hex and the stylesheet's
// app ground agreed only because nobody had changed one of them yet.
//
// So: the values live here, in DOM-free TypeScript that a node:test can read, and the other
// two surfaces are PINNED TO IT by test/theme.test.ts rather than by good intentions. The
// pin is a test plus a comment, deliberately — build-time CSS codegen would be a build step
// and a generated file to review for one shared seam (doc/design/ui-redesign.md, §Pinning).
//
// The design brief this implements — palette rationale, the three colour channels, the
// elevation model, the signature element, the type roles, and the maintainability rules
// every later slice is held to — is doc/design/ui-redesign.md. Read it before changing a
// value here; in particular, a hue is not free to move between channels.
//
// DOM-free on purpose: node:test imports this directly (no jsdom, no bundler).

/**
 * Two neutral ramps and eight named hues.
 *
 * `slate` is a deep, cool neutral — blue sits a few points above red at every step, so the
 * ground reads cool and recedes behind terminal output rather than tinting it. `mist` is
 * the ink. Both are unchanged from the direction the human approved: **colour enters this
 * palette through the foreground only, never by tinting the ground.**
 *
 * The eight hues serve THREE channels, not one (design note, §The three colour channels):
 * *state* (what an agent is doing), *interaction* (what the human can act on), and
 * *identity* (which thing this is — a surface, an action family, a CLI, a git lane). Four
 * of the eight also carry a state role; that is one pigment answering two questions, and
 * what keeps them apart is POSITION, not hue — see `IDENTITY` below.
 */
export const PALETTE = {
  // --- slate: the ground and the elevation ladder, deepest first. The four SURFACE steps
  //     are deliberately tiny — 1.041, 1.055, 1.087:1 between neighbours — because surfaces
  //     separate by elevation and a hairline, never by a contrast block. The two BORDER
  //     steps above them open up (1.146, 1.245:1): an edge has to be seen to do its job.
  slate000: "#0a0b0d", // terminal ground — the deepest surface in the app
  slate100: "#0f1114", // app ground (html/body, and the pre-paint hex in index.html)
  slate200: "#15171c", // panels, bars, headers, the rail
  slate300: "#1c1f26", // raised: cards, inputs, hovered rows, popovers
  slate400: "#262a32", // hairline borders
  slate500: "#343945", // strong borders, idle threads, disabled edges

  // --- mist: the ink. `mist400` is BELOW 4.5:1 on every ground by design — it is for
  //     non-essential meta and rules only. Anything a user must read uses mist200 or better.
  mist000: "#e7e9ee", // primary ink            (15.6:1 on slate100)
  mist200: "#9ba3b1", // secondary ink          (7.4:1)
  mist400: "#656d7b", // faint meta / dividers  (3.2-3.8:1 — non-text use only)

  // --- the eight hues, warm to cool around the wheel. Every one clears AA (4.5:1) as text
  //     on every slate ground including slate300, so a hue is legible on any surface
  //     without a per-surface exception; the worst case is azure at 5.01:1. Each carries a
  //     `Lit` step — the brighter companion used for ANSI bright, for hover emphasis, and
  //     for chip text sitting on a hairline of the same hue. There is deliberately no
  //     per-hue FILL: on this ground a chip is the ground plus a hairline, so a third
  //     value per hue would be a token nothing paints (design note, §The Lit step).
  rose: "#e8636f", //     state: danger / error / deletions
  roseLit: "#f4808a",
  amber: "#e8a94a", //    state: attention — this one needs you
  amberLit: "#f4c06a",
  lime: "#a9cc5a", //     identity only
  limeLit: "#c0dd7f",
  jade: "#45c08a", //     state: ok / done / additions
  jadeLit: "#6fd3a6",
  cyan: "#46bcd4", //     identity, and ANSI cyan
  cyanLit: "#74d3e5",
  azure: "#5590d9", //    state: working / live — and the one interaction accent
  azureLit: "#7fb0e8",
  azureDeep: "#24344a", // selection fill only — never text, never a border
  violet: "#a97fd6", //   identity, and ANSI magenta
  violetLit: "#c39ce8",
  orchid: "#e767a8", //   identity only
  orchidLit: "#f08cbd",

  // --- terminal-only. ANSI wants a true green in a slot where the app's greens are a teal
  //     (jade) and a yellow-green (lime); neither reads as "green" to a CLI, so ANSI green
  //     keeps its own pull. It is NOT an app hue — no UI surface may use it. `cyan` and
  //     `violet` used to live in this group and have been promoted to the identity channel,
  //     which is why it is now three names rather than seven.
  ansiGreen: "#57bd77",
  ansiGreenLit: "#74d190",
  ansiBlack: "#1e222a", //  above slate300 — dim, but visible on the terminal ground
} as const;

/**
 * The identity channel: hue used to say WHICH thing this is, not what state it is in.
 *
 * Surfaces that consume this: coloured icons and their role table, per-CLI marks, git-graph
 * lanes, diff add/delete, task-board columns, workflow-mode chrome, project tabs, resource
 * meters, and the syntax sub-palette. All eight hues are available to it, including the
 * four that also carry a state role — loomux has ONE palette, not two, and minting a second
 * near-identical blue so that "identity blue" could differ from "working blue" is exactly
 * the failure the token layer exists to prevent.
 *
 * WHAT KEEPS THE TWO CHANNELS APART IS POSITION, AND IT IS LOAD-BEARING. The state dyes
 * hold an exclusive claim to the state POSITIONS — the warp thread, the status chip, the
 * state dot — and an identity hue may never appear in one. That is not tidiness: under
 * simulated colour-vision deficiency this eight-hue set collapses (azure and violet differ
 * by 2.9 ΔE to a protanope; rose and orchid are identical to a tritanope), while the four
 * state dyes stay the most separable set under all three simulations. A supervisor who
 * cannot tell violet from azure still reads the fleet correctly, because nothing they must
 * act on was ever encoded in the channel that collapsed. `test/theme.test.ts` measures
 * exactly that and fails if identity ever becomes the more robust of the two.
 */
export const IDENTITY = {
  rose: PALETTE.rose,
  amber: PALETTE.amber,
  lime: PALETTE.lime,
  jade: PALETTE.jade,
  cyan: PALETTE.cyan,
  azure: PALETTE.azure,
  violet: PALETTE.violet,
  orchid: PALETTE.orchid,
} as const;

/** The `Lit` companion of each identity hue, in the same key order. */
export const IDENTITY_LIT = {
  rose: PALETTE.roseLit,
  amber: PALETTE.amberLit,
  lime: PALETTE.limeLit,
  jade: PALETTE.jadeLit,
  cyan: PALETTE.cyanLit,
  azure: PALETTE.azureLit,
  violet: PALETTE.violetLit,
  orchid: PALETTE.orchidLit,
} as const;

/**
 * Semantic roles. Surfaces consume THESE, never PALETTE directly — the whole point of the
 * layer is that "the colour of a paused agent" is a decision made once, here.
 */
export const SEMANTIC = {
  // The elevation ladder. Height above the ground means "closer to the human": the app
  // ground, then panels and the rail, then cards and popovers. Floating surfaces add a
  // shadow (styles.css) rather than a fourth colour.
  surfaceTerm: PALETTE.slate000,
  surface0: PALETTE.slate100,
  surface1: PALETTE.slate200,
  surface2: PALETTE.slate300,
  line: PALETTE.slate400,
  lineStrong: PALETTE.slate500,

  ink: PALETTE.mist000,
  inkDim: PALETTE.mist200,
  inkFaint: PALETTE.mist400,

  // Agent state. `held` and `idle` are ACHROMATIC on purpose: a held agent is not running,
  // so it carries no dye — it is marked by form (a dashed thread), not by hue. With eight
  // hues in the app, scarcity is no longer what makes these four mean something; POSITION
  // is. These six values are the only things allowed in a state position, and no identity
  // hue may enter one (see IDENTITY above).
  stateWorking: PALETTE.azure,
  stateAttention: PALETTE.amber,
  stateOk: PALETTE.jade,
  stateDanger: PALETTE.rose,
  stateHeld: PALETTE.mist400,
  stateIdle: PALETTE.slate500,

  // Interaction. One accent, used sparingly: the focus ring, the caret, the active tab, the
  // primary action. It is `azure` — the working dye — because the live agent and the thing
  // the human is acting on are the same idea, and a ninth hue would be a ninth meaning to
  // learn. Form separates them: state is an edge, interaction is a fill or a ring. The
  // accent stays ONE colour however many hues the identity channel gains: "what can I
  // click" must never become a hue-matching exercise.
  accent: PALETTE.azure,
  focus: PALETTE.azure,
  selection: PALETTE.azureDeep,
} as const;

/**
 * Type roles, not sizes. Sans carries labels and titles; mono is reserved for MACHINE
 * IDENTIFIERS — paths, branches, agent ids, counts, timings — so that the mono face means
 * "this is a literal string the machine gave you" wherever it appears.
 *
 * `mono` is byte-identical to the family chain pane.ts has always passed to xterm — changing
 * it would change cell metrics and force a refit (doc/design/xterm-resize-reflow.md). The
 * Cascadia faces ship with Windows Terminal / VS but are NOT guaranteed on the Windows 10
 * baseline; Consolas is, and carries the chain.
 */
export const FONT = {
  mono: '"Cascadia Code", "Cascadia Mono", Consolas, "Courier New", monospace',
  ui: '"Segoe UI Variable Text", "Segoe UI", system-ui, sans-serif',
} as const;

/** Terminal cell metrics. Pinned: any change here forces a one-time reflow of every pane. */
export const TERM_METRICS = { fontSize: 14, lineHeight: 1.1 } as const;

/**
 * The xterm.js ITheme, built from the palette above. pane.ts hands this straight to the
 * Terminal constructor — it holds no colours of its own.
 *
 * The 16 ANSI slots are asserted present and pairwise distinct by test/theme.test.ts: a
 * copy-paste typo that collapsed two slots would silently blank a colour for every CLI that
 * uses it, and nothing else in the app would notice.
 */
export const TERMINAL_THEME = {
  background: PALETTE.slate000,
  foreground: "#d5d9e1", // mist000 held back a touch — this is read for hours at a time
  cursor: PALETTE.azure, // the caret is interaction, so it takes the accent (SEMANTIC.focus)
  cursorAccent: PALETTE.slate000,
  selectionBackground: PALETTE.azureDeep,
  // xterm.js 6.0 replaced the native viewport scrollbar with its own widget
  // (see styles.css); these are the only scrollbar knobs it exposes.
  scrollbarSliderBackground: PALETTE.slate300,
  scrollbarSliderHoverBackground: PALETTE.slate400,
  scrollbarSliderActiveBackground: PALETTE.slate500,
  // Seven of the eight ANSI hues are now app hues rather than terminal-only inventions —
  // the identity channel needed a cyan and a magenta anyway, so the terminal and the chrome
  // finally speak the same palette. `lime` and `orchid` have no ANSI slot: they are app-only
  // hues, which is the reverse of the arrangement this replaced.
  black: PALETTE.ansiBlack,
  red: PALETTE.rose,
  green: PALETTE.ansiGreen,
  yellow: PALETTE.amber,
  blue: PALETTE.azure,
  magenta: PALETTE.violet,
  cyan: PALETTE.cyan,
  white: PALETTE.mist200,
  // ANSI bright-black is dimmed TEXT, so it is the faint ink, not a chrome edge — 3.8:1
  // on the terminal ground, which is the floor for something a CLI expects to be read.
  brightBlack: PALETTE.mist400,
  brightRed: PALETTE.roseLit,
  brightGreen: PALETTE.ansiGreenLit,
  brightYellow: PALETTE.amberLit,
  brightBlue: PALETTE.azureLit,
  brightMagenta: PALETTE.violetLit,
  brightCyan: PALETTE.cyanLit,
  brightWhite: PALETTE.mist000,
} as const;

/** The 16 ANSI slot names, in wire order. Exported so the test names what it checks. */
export const ANSI_SLOTS = [
  "black",
  "red",
  "green",
  "yellow",
  "blue",
  "magenta",
  "cyan",
  "white",
  "brightBlack",
  "brightRed",
  "brightGreen",
  "brightYellow",
  "brightBlue",
  "brightMagenta",
  "brightCyan",
  "brightWhite",
] as const;

/**
 * The CSS custom properties styles.css MUST declare, and the value each MUST carry.
 *
 * This is the pin, and it runs BOTH ways: test/theme.test.ts reads the `:root` block and
 * fails if any of these drifts, and it also fails on any raw colour declared in `:root`
 * that is missing from this map. Add a semantic colour token here when you add it to the
 * stylesheet — an unpinned token is a fourth place for the colours to disagree, and the
 * test will not let you mint one.
 */
export const CSS_TOKENS = {
  "--surface-term": SEMANTIC.surfaceTerm,
  "--surface-0": SEMANTIC.surface0,
  "--surface-1": SEMANTIC.surface1,
  "--surface-2": SEMANTIC.surface2,
  "--line": SEMANTIC.line,
  "--line-strong": SEMANTIC.lineStrong,
  "--ink": SEMANTIC.ink,
  "--ink-dim": SEMANTIC.inkDim,
  "--ink-faint": SEMANTIC.inkFaint,
  "--state-working": SEMANTIC.stateWorking,
  "--state-attention": SEMANTIC.stateAttention,
  "--state-ok": SEMANTIC.stateOk,
  "--state-danger": SEMANTIC.stateDanger,
  "--state-held": SEMANTIC.stateHeld,
  "--state-idle": SEMANTIC.stateIdle,
  "--accent": SEMANTIC.accent,
  "--focus": SEMANTIC.focus,
  "--selection": SEMANTIC.selection,
  // The identity channel. Four of these carry the same pigment as a `--state-*` token
  // above, and that duplication is the point: which token a surface names declares which
  // QUESTION it is answering, so a reviewer can see a channel violation in the diff without
  // knowing the hex. The `Lit` steps stay in theme.ts until a slice paints one — `:root`
  // carries what the stylesheet uses, not the whole palette.
  "--id-rose": IDENTITY.rose,
  "--id-amber": IDENTITY.amber,
  "--id-lime": IDENTITY.lime,
  "--id-jade": IDENTITY.jade,
  "--id-cyan": IDENTITY.cyan,
  "--id-azure": IDENTITY.azure,
  "--id-violet": IDENTITY.violet,
  "--id-orchid": IDENTITY.orchid,
  "--font-mono": FONT.mono,
  "--font-ui": FONT.ui,
} as const;

/**
 * The colour index.html must paint before the bundle arrives. Kept as its own export so the
 * pre-paint pin reads as what it is — the app's ground, not "whatever --surface-0 happens
 * to be today".
 */
export const PRE_PAINT_BACKGROUND = SEMANTIC.surface0;

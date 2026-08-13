// The loomux palette — the single source of truth for every colour loomux paints (#879).
//
// WHY THIS MODULE EXISTS. loomux's colours live in three languages that cannot see each
// other: the `:root` custom properties in styles.css, the critical pre-paint `background`
// in index.html (which must be right BEFORE the bundle loads, or the app flashes the wrong
// colour at startup), and the xterm.js ITheme in pane.ts (terminals render on a WebGL
// canvas — CSS custom properties are invisible to them). Before this module the three were
// hand-kept in sync, which is to say they were not: the pre-paint hex and `--bg-app` agreed
// only because nobody had changed one of them yet.
//
// So: the values live here, in DOM-free TypeScript that a node:test can read, and the other
// two surfaces are PINNED TO IT by test/theme.test.ts rather than by good intentions. The
// pin is a test plus a comment, deliberately — build-time CSS codegen would be a build step
// and a generated file to review for one shared seam (doc/design/ui-redesign.md, §Pinning).
//
// The design brief this implements — palette rationale, the elevation model, the signature
// element, the type roles, and the maintainability rules every later slice is held to — is
// doc/design/ui-redesign.md. Read it before changing a value here.
//
// DOM-free on purpose: node:test imports this directly (no jsdom, no bundler).

/**
 * Six named colours: two neutral ramps and four dyes.
 *
 * `slate` is a deep, cool neutral — blue sits a few points above red at every step, so the
 * ground reads cool and recedes behind terminal output rather than tinting it. `mist` is
 * the ink. The four dyes are the only saturated colour in the app and each means exactly
 * one thing; the accent is `azure`, the same dye as the working state, because in loomux
 * the live thing and the interactive thing are the same thing (design note, §Colour).
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

  // --- the four dyes. Each clears 4.5:1 on every slate ground up to slate300, so a dye is
  //     legible as text on any surface without a per-surface tint rule.
  azure: "#5590d9", //  working / live / the interaction accent
  azureLit: "#7fb0e8",
  azureDeep: "#24344a", // selection fill only — never text, never a border
  amber: "#e8a94a", //  attention — this one needs you
  amberLit: "#f4c06a",
  jade: "#45c08a", //   ok / done / additions
  jadeLit: "#6fd3a6",
  rose: "#e8636f", //   danger / error / deletions
  roseLit: "#f4808a",

  // --- terminal-only dyes. ANSI needs eight hues; the app needs four. These exist so the
  //     16-colour palette stays coherent with the app's, and are NOT app tokens: no UI
  //     surface may use them (the design note's palette is six named colours, not nine).
  violet: "#a97fd6", //     ANSI magenta
  violetLit: "#c39ce8",
  ansiGreen: "#57bd77", //  jade pulled toward green, so ANSI green reads as green
  ansiGreenLit: "#74d190",
  ansiCyan: "#42b3c9", //   jade pulled toward blue, so ANSI cyan reads as cyan
  ansiCyanLit: "#6ecbdd",
  ansiBlack: "#1e222a", //  above slate300 — dim, but visible on the terminal ground
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
  // so it carries no dye — it is marked by form (a dashed thread), not by hue. That keeps
  // saturated colour scarce and therefore meaningful.
  stateWorking: PALETTE.azure,
  stateAttention: PALETTE.amber,
  stateOk: PALETTE.jade,
  stateDanger: PALETTE.rose,
  stateHeld: PALETTE.mist400,
  stateIdle: PALETTE.slate500,

  // Interaction. One accent, used sparingly: the focus ring, the caret, the active tab, the
  // primary action. It is `azure` — the working dye — because the live agent and the thing
  // the human is acting on are the same idea, and a fifth hue would be a fifth meaning to
  // learn. Form separates them: state is an edge, interaction is a fill or a ring.
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
  black: PALETTE.ansiBlack,
  red: PALETTE.rose,
  green: PALETTE.ansiGreen,
  yellow: PALETTE.amber,
  blue: PALETTE.azure,
  magenta: PALETTE.violet,
  cyan: PALETTE.ansiCyan,
  white: PALETTE.mist200,
  // ANSI bright-black is dimmed TEXT, so it is the faint ink, not a chrome edge — 3.8:1
  // on the terminal ground, which is the floor for something a CLI expects to be read.
  brightBlack: PALETTE.mist400,
  brightRed: PALETTE.roseLit,
  brightGreen: PALETTE.ansiGreenLit,
  brightYellow: PALETTE.amberLit,
  brightBlue: PALETTE.azureLit,
  brightMagenta: PALETTE.violetLit,
  brightCyan: PALETTE.ansiCyanLit,
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
  "--font-mono": FONT.mono,
  "--font-ui": FONT.ui,
} as const;

/**
 * The colour index.html must paint before the bundle arrives. Kept as its own export so the
 * pre-paint pin reads as what it is — the app's ground, not "whatever --surface-0 happens
 * to be today".
 */
export const PRE_PAINT_BACKGROUND = SEMANTIC.surface0;

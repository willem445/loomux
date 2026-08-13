// The loom palette — the single source of truth for every colour loomux paints (#879).
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
// The design brief this implements — palette rationale, the signature element, the type
// roles, and the maintainability rules every later slice is held to — is
// doc/design/ui-redesign.md. Read it before changing a value here.
//
// DOM-free on purpose: node:test imports this directly (no jsdom, no bundler).

/**
 * The six named threads, plus their tints. Six dyes and one undyed fibre — the whole app
 * is woven from these; nothing else may introduce a colour (see the maintainability rules
 * in the design note).
 *
 * `fibre` is the undyed ground: a warm, near-achromatic dark (R > G > B by a few points at
 * ~3% chroma) rather than the blue-black every agent tool reaches for. `linen` is the ink.
 * The four dyes carry MEANING and nothing else — indigo/saffron/verdigris/madder are agent
 * state, never decoration.
 */
export const PALETTE = {
  // --- fibre: the undyed ground, darkest first. The four SURFACE steps are deliberately
  //     tiny — 1.047, 1.060, 1.083:1 between neighbours — because surfaces separate by a
  //     hairline and by spacing, never by a heavy contrast block. The two BORDER steps
  //     above them open up (1.123, 1.177:1): an edge has to be seen to do its job.
  fibre000: "#0f0e0b", // terminal ground — the deepest surface in the app
  fibre100: "#15140f", // app ground (html/body, and the pre-paint hex in index.html)
  fibre200: "#1c1a14", // panels, bars, headers
  fibre300: "#24211a", // raised: inputs, hovered rows, popovers
  fibre400: "#2e2a20", // hairline borders
  fibre500: "#3b352a", // strong borders, idle threads, disabled edges

  // --- linen: the ink. `linen400` is BELOW 4.5:1 on every ground by design — it is for
  //     non-essential meta and rules only. Anything a user must read uses linen200 or better.
  linen000: "#e4dfd2", // primary ink            (13.9:1 on fibre100)
  linen200: "#a49d8b", // secondary ink          (6.8:1)
  linen400: "#767162", // faint meta / dividers  (3.3-4.0:1 — non-text use only)

  // --- the four dyes. Each clears 4.5:1 on every fibre ground up to fibre300, so a dye is
  //     legible as text on any surface without a per-surface tint rule.
  indigo: "#7687d6", //  working / in flight / info
  indigoLit: "#8d9ce4",
  indigoDeep: "#2f3350", // selection fill only — never text, never a border
  saffron: "#e2a33c", // the human's attention: needs-you, focus ring, caret
  saffronLit: "#f2bb5c",
  verdigris: "#4fae94", // ok / done / additions
  verdigrisLit: "#6fc6ab",
  madder: "#dd665e", //  danger / error / deletions
  madderLit: "#ef7c72",

  // --- terminal-only dyes. ANSI needs eight hues; the app needs four. These two exist so
  //     the 16-colour palette stays coherent with the app's, and are NOT app tokens: no UI
  //     surface may use them (the design note's palette is six named threads, not eight).
  murex: "#b57ec9", //   ANSI magenta
  murexLit: "#c895d8",
  ansiGreen: "#5fae7c", //  verdigris pulled toward green, so ANSI green reads as green
  ansiGreenLit: "#7cc796",
  ansiCyan: "#45b3b8", //   verdigris pulled toward blue, so ANSI cyan reads as cyan
  ansiCyanLit: "#63cbcf",
  ansiBlack: "#2a261d", // between fibre200 and fibre300 — visible on the terminal ground
} as const;

/**
 * Semantic roles. Surfaces consume THESE, never PALETTE directly — the whole point of the
 * layer is that "the colour of a paused agent" is a decision made once, here.
 */
export const SEMANTIC = {
  surfaceTerm: PALETTE.fibre000,
  surface0: PALETTE.fibre100,
  surface1: PALETTE.fibre200,
  surface2: PALETTE.fibre300,
  line: PALETTE.fibre400,
  lineStrong: PALETTE.fibre500,

  ink: PALETTE.linen000,
  inkDim: PALETTE.linen200,
  inkFaint: PALETTE.linen400,

  // Agent state. `held` and `idle` are ACHROMATIC on purpose: a held agent is not running,
  // so it carries no dye — it is marked by form (a dashed thread), not by hue. That keeps
  // saturated colour scarce and therefore meaningful.
  stateWorking: PALETTE.indigo,
  stateAttention: PALETTE.saffron,
  stateOk: PALETTE.verdigris,
  stateDanger: PALETTE.madder,
  stateHeld: PALETTE.linen400,
  stateIdle: PALETTE.fibre500,

  // Where the human is, or is wanted. One hue for both; form tells them apart (design note,
  // §The warp). Chrome carries no other hue.
  focus: PALETTE.saffron,
  selection: PALETTE.indigoDeep,
} as const;

/**
 * Type roles, not sizes. The chrome is monospace-forward: everything that is DATA (pane
 * names, branches, ids, counts, timings) is set in the terminal's own face, so chrome and
 * content read as one cloth. `ui` is for prose only.
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
  background: PALETTE.fibre000,
  foreground: "#d6d1c3", // linen000 held back a touch — this is read for hours at a time
  cursor: PALETTE.saffron, // saffron marks where the human is (SEMANTIC.focus)
  cursorAccent: PALETTE.fibre000,
  selectionBackground: PALETTE.indigoDeep,
  // xterm.js 6.0 replaced the native viewport scrollbar with its own widget
  // (see styles.css); these are the only scrollbar knobs it exposes.
  scrollbarSliderBackground: PALETTE.fibre300,
  scrollbarSliderHoverBackground: PALETTE.fibre400,
  scrollbarSliderActiveBackground: PALETTE.fibre500,
  black: PALETTE.ansiBlack,
  red: PALETTE.madder,
  green: PALETTE.ansiGreen,
  yellow: PALETTE.saffron,
  blue: PALETTE.indigo,
  magenta: PALETTE.murex,
  cyan: PALETTE.ansiCyan,
  white: PALETTE.linen200,
  // ANSI bright-black is dimmed TEXT, so it is the faint ink, not a chrome edge — 4.0:1
  // on the terminal ground, which is the floor for something a CLI expects to be read.
  brightBlack: PALETTE.linen400,
  brightRed: PALETTE.madderLit,
  brightGreen: PALETTE.ansiGreenLit,
  brightYellow: PALETTE.saffronLit,
  brightBlue: PALETTE.indigoLit,
  brightMagenta: PALETTE.murexLit,
  brightCyan: PALETTE.ansiCyanLit,
  brightWhite: PALETTE.linen000,
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

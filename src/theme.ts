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
 * Two neutral ramps, eight named hues, and a seven-pigment per-CLI set that answers one
 * closed question in one position (see `CLI_HUES`).
 *
 * `slate` is ACHROMATIC — `r === g === b` at every step, and so is `mist`. It used to lean
 * cool (blue 3-17 points above red as the ramp climbed), which read as a blue cast across
 * the whole chrome; #1320 ask 1 removed it. Each step was collapsed to its own GREEN channel,
 * which holds luminance nearly constant (luminance is 71.52% green), so the elevation ladder
 * and every contrast ratio survived the change while the hue did not — the largest step
 * moved 1.4%. `mist` is the ink, and it is the "silver" half of the black-and-gold identity.
 * The older rule is unchanged and now literally true: **colour enters this palette through
 * the foreground only, never by tinting the ground.**
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
  slate000: "#0b0b0b", // terminal ground — the deepest surface in the app
  slate100: "#111111", // app ground (html/body, and the pre-paint hex in index.html)
  slate200: "#171717", // panels, bars, headers, the rail
  slate300: "#1f1f1f", // raised: cards, inputs, hovered rows, popovers
  slate400: "#2a2a2a", // hairline borders
  slate500: "#393939", // strong borders, idle threads, disabled edges

  // --- mist: the ink. `mist400` is BELOW 4.5:1 on every ground by design — it is for
  //     non-essential meta and rules only. Anything a user must read uses mist200 or better.
  //
  //     mist000 was toned down from its original 15.6:1 (#e7e9ee) at the human's request —
  //     "a little extreme" against the near-black grounds (#1020 item 11). The candidate
  //     matrix considered, measured against slate100 (the app ground) with the identical
  //     WCAG formula test/theme.test.ts runs, all comfortably clear the ramp's own AAA floor
  //     (>=7:1 on every ground, test: "the ink ramp keeps the contrast the design note
  //     promises") with room to spare even on slate300, the lightest ground it sits on:
  //
  //     The candidates below are the #1020 set carried through #1320's de-blueing — each
  //     collapsed to its own green channel, which is what held the ramp's luminance while
  //     the hue left. That the ratios barely moved (13.50 -> 13.51, 12.49 -> 12.49,
  //     11.53 -> 11.52) IS the evidence for that claim, not a coincidence:
  //
  //       candidate      hex        surface0   surface1   surface2   surfaceTerm
  //       mild trim      #dadada    13.51:1    12.82:1    11.79:1    14.08:1
  //       DEFAULT (mid)  #d2d2d2    12.49:1    11.86:1    10.90:1    13.02:1   <- shipped
  //       strong trim    #cacaca    11.52:1    10.94:1    10.06:1    12.01:1
  //
  //     Terminal consequence: none of these candidates touch TERMINAL_THEME.brightWhite
  //     (below) — it is its own literal, not PALETTE.mist000, precisely so that swapping the
  //     candidate here can never again silently re-paint the terminal's bright-white slot. It
  //     did once: shipping DEFAULT while brightWhite aliased mist000 put brightWhite (L
  //     0.6437) BELOW `foreground` (L 0.6921), inverting bright-white emphasis in every pane
  //     (#1033 review) — see the brightWhite comment in TERMINAL_THEME.
  //
  //     DEFAULT was picked as the midpoint of the requested ~12-13:1 band. Which candidate
  //     reads right is a human call at the demo (#1020 human input 4) — swap this one hex
  //     to move the whole app; nothing else here needs to change (styles.css / index.html
  //     stay pinned to whichever value lands here). The surface ladder itself (below) was
  //     deliberately left untouched: its steps are already at the finest gap 8-bit hex can
  //     express at this luminance (adjacent hex values differ by ~0.02:1 of contrast here),
  //     so softening it further either does nothing visible or risks breaking the strictly-
  //     increasing elevation order for no perceptible gain — a call for a design slice with
  //     room to re-derive the whole ladder, not a same-day tone-down.
  mist000: "#d2d2d2", // primary ink            (12.5:1 on slate100)
  mist200: "#a3a3a3", // secondary ink          (7.4:1)
  mist400: "#6d6d6d", // faint meta / dividers  (3.2-3.8:1 — non-text use only)

  // --- the eight hues, warm to cool around the wheel. Every one clears AA (4.5:1) as text
  //     on every slate ground including slate300, so a hue is legible on any surface
  //     without a per-surface exception; the worst case is rose at 4.61:1. Each carries a
  //     `Lit` step — the brighter companion used for ANSI bright, for hover emphasis, and
  //     for chip text sitting on a hairline of the same hue. There is deliberately no
  //     per-hue FILL: on this ground a chip is the ground plus a hairline, so a third
  //     value per hue would be a token nothing paints (design note, §The Lit step).
  rose: "#e35a5a", //     state: danger / error / deletions — a TRUE red (hue 0), not the
  roseLit: "#ee8080", //   pink-leaning rose it replaces (#1320 ask 3, "red = error/dead")
  amber: "#e89c3c", //    state: attention — pulled toward orange from its old yellow-orange,
  amberLit: "#f2b45f", //  as far toward it as tritanopia allows (see SEMANTIC.stateAttention)
  lime: "#b5bf62", //     identity only — brass, down from a candy yellow-green
  limeLit: "#cbd486",
  jade: "#35a86b", //     state: ok / done / additions
  jadeLit: "#56c48c",
  cyan: "#5aa8b5", //     identity, and ANSI cyan — muted teal, not the old bright cyan
  cyanLit: "#7ec3ce",
  azure: "#6f93c4", //    identity, and ANSI blue. NO LONGER a state dye nor the accent:
  azureLit: "#95b3d8", //  after #1320 this pigment never paints chrome, only a lane or glyph
  violet: "#9a8fc4", //   identity, and ANSI magenta — slate-violet, down from a lilac
  violetLit: "#b7aed8",
  orchid: "#c47f9e", //   identity only — dusty rose, down from a hot pink
  orchidLit: "#d8a2bb",

  // --- the interaction pigment, and the one state pigment that is not also an identity hue.
  //
  //     gold is the BRAND accent (#1320 ask 2): black + gold/silver is the post-rebrand
  //     identity, and this is the orrery gold off the logo, tempered a little off its
  //     #ffc82a core so that a caret and a focus ring are not the brightest thing on the
  //     screen. Before #1320 the interaction channel had no pigment of its own — it
  //     borrowed `azure` from the state channel, which is why "what can I click" and "what
  //     is this doing" were the same colour. There is no `silver` token: the silver half of
  //     the identity IS the `mist` ramp, which is now genuinely achromatic.
  //
  //     spring is `working`. It exists because the direction puts BOTH running and done on
  //     green, and two states may never share a pigment — so `jade` keeps `ok` and the
  //     brighter, mintier `spring` takes the live thread. It sits 10.1 dE from `ansiGreen`
  //     below: two near-identical greens in one palette is the smell the token layer exists
  //     to prevent, so this one is far enough away to be its own pigment.
  gold: "#f8c93e", //     interaction: the accent, the focus ring, the caret
  spring: "#74d98d", //   state: working / live
  selectionFill: "#38321f", // selection fill only — never text, never a border.
  //     It was `azureDeep`, sitting inside the azure block; #1320 made it a deep GOLD wash,
  //     and a name is what the next person greps. A future azure re-tune must not find this.
  //     Its LEVEL is set by the two jobs a selection has, and both were measured against the
  //     value it replaces rather than eyeballed: it must be visible as a fill on the app
  //     ground (1.48:1, against the old 1.50:1) AND text must stay readable on top of it
  //     (ink 8.45:1, against the old 8.33:1). The first draft was #2e2a1c, which kept the
  //     text side but dropped the fill to 1.32:1 — a selection you can read but can barely
  //     see is still a regression.

  // --- the per-CLI brand hues (#1020 wave 2). Seven pigments that exist for ONE position —
  //     the agent-type mark and the session list's CLI chip — and answer one closed
  //     question: *which program is this pane running*. They are not a ninth..fifteenth
  //     identity hue and they do not compete with the eight above; see `CLI_HUES` for why
  //     they had to be their own set rather than borrowed `--id-*` tokens.
  //
  //     EVOCATION, NOT REPLICATION. Where a vendor has a well-known palette the pigment
  //     leans toward it — clay for Anthropic's warm terracotta, teal for OpenAI's green,
  //     steel for GitHub's blue, indigo for Gemini's blue-violet — but no value here is a
  //     vendor's own hex, and none is presented as one. A trademark colour copied exactly
  //     is a claim of affiliation loomux does not make and does not need: the same
  //     nominative-use reasoning that lets agenticons.ts draw GitHub's own glyph (§Licensing
  //     there) is why it may lean toward GitHub's blue without taking it. The three CLIs
  //     whose vendors publish no colour identity at all (opencode, hermes, ante) get hues
  //     loomux picked outright, for separation and nothing else.
  //
  //     No `Lit` step: unlike the eight, nothing paints an emphasis tier of a CLI hue — a
  //     mark is one flat glyph and a chip is a 16% wash of the base. A step nothing paints
  //     is a token the pin cannot check (design note, §The Lit step).
  clay: "#e08a5f", //     claude   — warm terracotta
  citron: "#c3c455", //   ante     — yellow-green
  fern: "#5fc873", //     opencode — a true green
  teal: "#3ec2a8", //     codex    — green-cyan
  steel: "#7fa8d8", //    copilot  — a cool desaturated blue
  indigo: "#9677c5", //   gemini   — blue-violet, muted at #1320 ask 3
  fuchsia: "#b67c94", //  hermes   — dusty mauve, muted at #1320 ask 3

  // --- terminal-only. ANSI wants a true green in a slot where the app's greens are a teal
  //     (jade) and a yellow-green (lime); neither reads as "green" to a CLI, so ANSI green
  //     keeps its own pull. It is NOT an app hue — no UI surface may use it. `cyan` and
  //     `violet` used to live in this group and have been promoted to the identity channel,
  //     which is why it is now three names rather than seven.
  ansiGreen: "#57bd77",
  ansiGreenLit: "#74d190",
  ansiBlack: "#222222", //  above slate300 — dim, but visible on the terminal ground
} as const;

/**
 * The identity channel: hue used to say WHICH thing this is, not what state it is in.
 *
 * Surfaces that consume this: coloured icons and their role table, per-CLI marks, git-graph
 * lanes, diff add/delete, task-board columns, workflow-mode chrome, project tabs, resource
 * meters, and the syntax sub-palette. All eight hues are available to it, including the
 * three that also carry a state role — loomux has ONE palette, not two, and minting a second
 * near-identical pigment so that the identity copy could differ from the state copy is
 * exactly the failure the token layer exists to prevent. (The rule is stated without naming
 * a hue on purpose: it used to read "identity blue" versus "working blue", which #1320
 * falsified when `working` moved off azure onto `spring`. The failure mode is not about
 * blue.)
 *
 * WHAT KEEPS THE TWO CHANNELS APART IS POSITION, AND IT IS LOAD-BEARING. The state dyes
 * hold an exclusive claim to the state POSITIONS — the warp thread, the status chip, the
 * state dot — and an identity hue may never appear in one. That is not tidiness: under
 * simulated colour-vision deficiency this eight-hue set collapses (azure and violet are
 * INDISTINGUISHABLE to a protanope at 0.0 ΔE; cyan and azure are 2.4 ΔE to a tritanope, and
 * rose and orchid 4.3 ΔE), while the four state dyes stay the most separable set under all
 * three simulations — their own worst case is 10.8 ΔE. A supervisor who
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
 * §The per-CLI hues — program name → the pigment that says *which CLI this pane runs*.
 *
 * WHY THIS EXISTS AT ALL. Before this table every agent pane in the app was the same
 * violet: the mark took the `fleet` icon role's dye (`--id-violet`, "the agents
 * themselves"), which is the correct answer to *is this an agent?* and no answer at all to
 * *which one?* — the question the mark was added to answer (#992). A wall of ten panes
 * running three different CLIs came out one colour, so the glyph had to be read rather than
 * seen, which is exactly what the mark exists to avoid.
 *
 * WHY IT COULD NOT REUSE `--id-*`, WHICH IS THE ONLY INTERESTING DECISION HERE. The eight
 * identity hues are in BIJECTION with the eight icon roles — each hue claimed by exactly one
 * role, enforced in both directions by test/icons.test.ts, and that bijection is what stops
 * eight hues from decaying into a palette of nice colours. Handing `--id-jade` to opencode
 * would not add a meaning, it would give jade a SECOND one, and "jade" would stop resolving
 * to `content` — the failure the bijection was written to prevent. So a per-CLI hue needs a
 * pigment no icon role has claimed, which means a new set, which means a new prefix. That
 * prefix is also the reviewable signal: `--cli-*` in a diff says "this surface is answering
 * *which program*", the same way `--state-*` vs `--id-*` already declares state vs identity.
 *
 * THEY ARE STILL THE IDENTITY CHANNEL, NOT A FOURTH ONE. "Which CLI is this" is the
 * identity question by definition (design note, §The three colour channels, which already
 * lists "per-CLI marks" as an identity consumer). `--cli-*` is a SUB-TABLE of that channel
 * for one closed roster, not a new channel with new rules: an identity hue may still never
 * enter a state position, and neither may one of these.
 *
 * WHY SEVEN MORE PIGMENTS DOES NOT BREAK "EIGHT IS A MEASUREMENT". That ceiling was measured
 * for hues that must be told apart ACROSS THE WHOLE APP — a ninth would have landed closer to
 * an existing hue than the eight-set's own closest pair (azure/violet, 15.3 ΔE). These seven
 * never have to survive that comparison, because they only ever appear in ONE position
 * against each other: the agent mark and the session list's CLI chip. Measured on their own
 * terms they are legible — closest pair 31.5 ΔE (codex/opencode) — and test/theme.test.ts
 * holds them to a floor derived from the eight rather than hard-coded.
 *
 * THAT COMPARISON REVERSED AT #1320, and the argument has to be restated rather than have its
 * number swapped. It used to read "tighter than the eight's own 30.4", which was a licence:
 * seven extra pigments were justified BECAUSE they were the more legible set. De-exoticising
 * the octet dropped its closest pair to 15.3, so the CLI set is now the LOOSER comparison —
 * 31.5 clears 15.3 with room, but it clears it the way any adequate set clears a lowered bar,
 * not by being exemplary. What still justifies the seven is the narrower claim underneath:
 * they answer one closed question in two positions and never have to be told apart from the
 * eight, only from each other. If a future round tightens the octet back up, this paragraph
 * is the one to re-derive.
 *
 * COLOUR-VISION DEFICIENCY, HONESTLY — AND THE WORST CASE IS TRITAN, NOT THE RED-GREEN ONES.
 * Seven hues on one ground do not survive CVD and these do not. Closest pair per simulation:
 * protan opencode/ante 15.6, deutan codex/hermes 9.5, and tritan codex/copilot **1.4** —
 * which is the honest headline, because a tritanope sees those two as one colour. (Every
 * figure here and below is CIE76 over the Viénot LMS simulation in test/theme.test.ts; that
 * is the only method quoted anywhere in this feature, so two surfaces cannot disagree by
 * having measured differently.)
 *
 * That collapse is the same trade the identity channel already makes and states — state dyes
 * stay separable, identity does not have to, because identity is also carried by position,
 * label and SHAPE. Here the shape is usually one letter, so the trade holds only while no two
 * CLIs draw the same one; where they do, colour is the last channel and has to survive what
 * the others are excused from. Today that is exactly one pair — claude and codex both badge
 * `C` — and its worst view is 25.1 ΔE (protan; deutan 42.2, tritan 135.4). Every pair that
 * DOES collapse is shape-distinct: codex/copilot is a `C` against the vendored octicon,
 * claude/hermes (3.8 tritan) a `C` against an `H`. test/theme.test.ts computes the collision
 * set from the renderer rather than hard-coding it, so an eighth CLI starting with `C`
 * inherits the obligation automatically.
 *
 * Keys are program names as `normalizeAgentProgram` spells them, which is what lets
 * src/agenticons.ts stamp `cli-<program>` without a second table to keep in step;
 * test/agenticons.test.ts pins the two lists against each other in both directions.
 */
export const CLI_HUES = {
  claude: PALETTE.clay,
  codex: PALETTE.teal,
  copilot: PALETTE.steel,
  opencode: PALETTE.fern,
  gemini: PALETTE.indigo,
  hermes: PALETTE.fuchsia,
  ante: PALETTE.citron,
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
  // The scale is the standard one a supervisor already knows (#1320 ask 3): green is
  // running and green is done, orange needs you, red is broken, and a stopped agent has no
  // hue at all. `attention` is as far toward a true orange as it can go: past roughly hue
  // 34 it converges with `danger` for a tritanope (both lose the yellow axis), and the
  // 9 dE floor below is the thing that actually has to hold, so the last few degrees of
  // orange are spent on staying distinguishable rather than on being more orange.
  // Measured: the state worst case is 10.8 ΔE (tritan, attention/danger). It was 10.3 before
  // #1320, so retuning the scale improved the load-bearing measurement rather than spending it.
  stateWorking: PALETTE.spring,
  stateAttention: PALETTE.amber,
  stateOk: PALETTE.jade,
  stateDanger: PALETTE.rose,
  stateHeld: PALETTE.mist400,
  stateIdle: PALETTE.slate500,

  // Interaction. One accent, used sparingly: the focus ring, the caret, the active tab, the
  // primary action. It is `gold` — the brand pigment — and it is NOT any state dye.
  //
  // It used to be `azure`, which was also `stateWorking`, on the argument that the live
  // agent and the thing the human is acting on "are the same idea". They are not: a pane
  // can be working while the thing you may click is somewhere else entirely, and sharing
  // one pigment made that unaskable in colour. Splitting them is what #1320 ask 2 and ask 3
  // require of each other — a gold accent is only legible AS an accent if the semantic
  // scale it sits beside is something else. `test/theme.test.ts` ("the brand accent is not
  // a state dye") holds the two apart at the state channel's own 9 dE floor, under CVD too.
  //
  // Form still separates as before: state is an edge, interaction is a fill or a ring. The
  // accent stays ONE colour however many hues the identity channel gains: "what can I
  // click" must never become a hue-matching exercise. Used SPARINGLY is part of the token:
  // gold on every surface is not an accent, it is a theme.
  // Measured: the accent sits 12.8 ΔE from the nearest state dye at its worst (tritan,
  // against attention). On the pre-#1320 palette that distance was 0.0 — it WAS the dye.
  accent: PALETTE.gold,
  focus: PALETTE.gold,
  selection: PALETTE.selectionFill,
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
  foreground: "#d9d9d9", // independent literal, not derived from mist000 — read for hours at a time
  cursor: PALETTE.gold, // the caret is interaction, so it takes the accent (SEMANTIC.focus)
  cursorAccent: PALETTE.slate000,
  selectionBackground: PALETTE.selectionFill,
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
  // Independent literal, NOT PALETTE.mist000: the primary-ink tone-down (#1020 item 11) must
  // never carry into ANSI bright-white, or a later mist000 edit silently dims the terminal's
  // brightest slot below `foreground` (#d5d9e1, L 0.6921) — exactly what aliasing this to
  // mist000 once did (L 0.6437 < 0.6921), inverting bright-white emphasis in every pane
  // (#1033 review). Kept at mist000's pre-tone-down value so brightWhite stays the brightest
  // thing a CLI can print.
  brightWhite: "#e9e9e9",
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
  // The identity channel. Three of these carry the same pigment as a `--state-*` token
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
  // The per-CLI sub-table (see CLI_HUES). A separate prefix rather than eight more `--id-*`
  // entries, because `--id-*` is bijective with the icon role table and a CLI claiming one
  // would give that hue a second meaning. Every key here is a program name, so the token a
  // rule names IS the program it dyes — there is no third spelling to drift.
  "--cli-claude": CLI_HUES.claude,
  "--cli-codex": CLI_HUES.codex,
  "--cli-copilot": CLI_HUES.copilot,
  "--cli-opencode": CLI_HUES.opencode,
  "--cli-gemini": CLI_HUES.gemini,
  "--cli-hermes": CLI_HUES.hermes,
  "--cli-ante": CLI_HUES.ante,
  "--font-mono": FONT.mono,
  "--font-ui": FONT.ui,
} as const;

/**
 * The colour index.html must paint before the bundle arrives. Kept as its own export so the
 * pre-paint pin reads as what it is — the app's ground, not "whatever --surface-0 happens
 * to be today".
 */
export const PRE_PAINT_BACKGROUND = SEMANTIC.surface0;

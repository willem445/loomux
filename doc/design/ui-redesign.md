# loomux's visual and experience design brief (#879)

This is the brief the whole redesign line is built to. Slice A ships the token layer and
this document; slices B through J restyle surfaces against it. Where a later slice and this
note disagree, one of them is wrong and it has to be settled here first.

Read it as an argument, not a spec sheet. Every value below exists because of a reason
stated next to it, and a reason you disagree with is a reason to change the value.

## What this is for

loomux runs a fleet of coding agents. On a busy day that is eight or ten terminals, most of
them working, one of them stuck, one of them finished and waiting on you. The job of the
chrome is one sentence: **tell the human which thread needs their hands, without ever
getting in front of the terminal that is doing the work.**

Everything here is downstream of that. The chrome is not the product; the panes are.

## The direction, and where it came from

The direction is set by the human, twice over. The issue names **ORCA** (onorca.dev) and
**T3 Code** (github.com/pingdotgg/t3code) as the target feel. A first proposal in a
different direction — a warm, undyed-flax ground — was **rejected at the direction gate**:
*"the UI color scheme is terrible. Not a fan of the yellowish black color. I'm looking for a
similar look and feel to ORCA + T3 … not just a color refresh but also a redesign on the
experience."* So this revision is cool-neutral, and it covers experience as well as colour.

Six principles come out of the references — from their public pages, docs, public design
discussion, and a demo screenshot of ORCA the human supplied as the target:

1. **Chrome recedes; the work is the content.** Surfaces sit close together in luminance and
   separate by elevation and a hairline, not by heavy contrast blocks.
2. **Elevation is the organising idea.** Ground, panels, cards — and the surfaces that
   genuinely float say so with a radius and a shadow, while the ones that tile stay flat and
   tight. Depth carries "how close is this to me", so colour does not have to.
3. **Density is respect, with air.** Many agents' states at once, but rows get breathing
   room and sections get separation. Dense is not cramped.
4. **Structure is labelled, quietly.** Tiny letterspaced uppercase eyebrows over grouped
   content, small status chips on rows, machine strings in mono. The structure does the
   explaining so the copy does not have to.
5. **State is colour; colour is state.** Status reads as small consistent colour signals
   rather than as boxy badge components — and the corollary most tools miss: if colour means
   state, nothing that is *not* state may take colour. Accent usage is restrained.
6. **Supervision beats editing.** The question the UI answers is "what is happening, and
   where do I have to look", not "how do I edit this file".

**The no-copy line — binding on every slice of #879.** The references are studied for
principles only. No palette value, logo, layout signature, icon set, or distinctive
component from ORCA, T3 Code, or any other product may be lifted, traced, or approximated.
Every hex in this document was derived here; each was checked for zero occurrences in the
T3 Code source (`gh search code <hex> --repo pingdotgg/t3code`), and the check is recorded in
the PR. "Look and feel like ORCA" means *speak the same visual language*, and the sharpest
test of that is §The signature: the single most distinctive object in the reference is the
one thing this design may not reach for.

The same discipline applies to *themes*: the palette this replaces was Tokyo Night's, hex
for hex, in `TERM_THEME`. Borrowing a well-liked theme is how an app ends up with someone
else's identity and no argument for any of it.

## The palette — six named colours

Dark-only, and staying that way: `color-scheme: dark` on `:root` keeps native widgets
(select popups, scrollbars) dark, and a light theme would double every demo surface for a
product whose users work at night against terminals. The tokens make one *possible* later;
this line does not build one.

| Name | Value | Role |
| --- | --- | --- |
| **slate** | `#0a0b0d` → `#343945` | the ground and the elevation ladder; every surface and border |
| **mist** | `#e7e9ee` / `#9ba3b1` / `#656d7b` | the ink: primary, secondary, faint |
| **azure** | `#5590d9` | agent **working** — and the one interaction accent |
| **amber** | `#e8a94a` | **attention** — this one needs you |
| **jade** | `#45c08a` | **ok** — done, passing, additions |
| **rose** | `#e8636f` | **danger** — failed, error, deletions |

**The ground is a deep cool neutral.** Blue sits a few points above red at every step of the
slate ramp (`B−R` = 3, 5, 7, 10, 12, 17 as it climbs), so the chrome reads cool and recedes
behind terminal output instead of tinting it. This is the correction the direction gate
asked for, and it is the *opposite* of the rejected proposal, which warmed the same channel
in the other direction.

**One accent, and it is the working dye.** `--accent` is azure — the same colour as an agent
in flight — because in loomux the live thing and the thing the human is acting on are the
same thing, and a fifth hue would be a fifth meaning to learn. Form keeps them apart: state
is an *edge*, interaction is a *fill or a ring*. The accent appears on the focus ring, the
caret, the active tab and the primary action, and nowhere else; everything else in the chrome
is slate and mist.

**Stopped agents get no dye.** `held` is faint mist and `idle` is a slate hairline. A held
agent is not running, so it carries no dye; it is marked by *form* — a dashed thread — not
by hue. This also keeps the state set legible for red/green colour blindness: the two states
a supervisor must never confuse are "needs you" (amber) and "working" (azure), which differ
in hue, lightness and position.

The terminal needs eight hues where the app needs four, so `theme.ts` carries three
terminal-only dyes — **violet** (`#a97fd6`, ANSI magenta) and two pulls of jade toward green
(`#57bd77`) and toward blue (`#42b3c9`) so ANSI green reads as green and ANSI cyan as cyan.
These are not app tokens. No UI surface may use them.

**Contrast is measured, not claimed.** `test/theme.test.ts` computes WCAG ratios over these
values on every run: primary ink clears AAA (7:1) on all four grounds, dim ink clears AA,
each of the four dyes clears AA (4.5:1) on every surface it can appear on — the worst case is
azure at 5.01:1 on `--surface-2` — and faint mist is *held* between 3:1 and 4.5:1,
deliberately below AA, because its role is non-essential meta. If a future edit makes faint
ink readable, it has stopped being a separate role and the test says so.

## Elevation — the model, not a decoration

Four surfaces, one ladder, and the rule that governs it: **height means "closer to the
human".**

| Level | Token | What sits here |
| --- | --- | --- |
| ground | `--surface-term` | terminal canvases — the deepest thing on screen |
| 0 | `--surface-0` | the app ground behind everything |
| 1 | `--surface-1` | panels, bars, headers, the rail |
| 2 | `--surface-2` | cards, inputs, hovered rows, popovers |

The steps between them are tiny by measurement — 1.041, 1.055 and 1.087:1 — because
principle 1 says surfaces separate by elevation and a hairline, not by a slab;
`test/theme.test.ts` fails a surface step above 1.3:1. The two border steps above the ramp
deliberately open up (1.146:1 and 1.245:1), because an edge that nobody can see is not doing
the separating the surfaces are refusing to do.

Shadows are the second half of the model and they are rationed: `--shadow-card` for a raised
object, `--shadow-float` for one that genuinely floats over the work. **Neither may sit under
permanent chrome** — a shadow under a bar that is always there is decoration — and **no large
soft shadow may be composited over a terminal**, which is the documented way to make this app
slow (`doc/design/performance.md`). The terminal is the one surface with no elevation at all:
it is the floor.

## Type

Two roles, both system-resident. **No webfont is vendored, and that is an argument, not a
default.**

- `--font-ui` — `"Segoe UI Variable Text", "Segoe UI", system-ui, sans-serif`. Labels,
  titles, prose, buttons: everything a person wrote.
- `--font-mono` — `"Cascadia Code", "Cascadia Mono", Consolas, "Courier New", monospace`.
  **Machine identifiers only**: paths, branches, agent ids, model names, counts, timings,
  keycaps. The rule is what makes it work — wherever the mono face appears, it means "this is
  a literal string the machine gave you", so the eye can skip it or trust it accordingly.

Vendoring a characterful OFL face is the obvious move for "make it look designed", and this
brief still refuses it: the window paints before the bundle exists (see §Pinning), so a
webfont buys a flash of the wrong face on every cold start, on a surface whose whole job is
to recede. The character comes from the *treatment* instead — the eyebrow.

**The eyebrow** is the structural device this design commits to: a 10px uppercase label at
`0.09em` tracking in faint mist, sitting over a group of content, with no rule and no box.
It is how sections announce themselves without chrome, and it is the reason the boards and
the cards can drop most of their borders. Section labels are nouns and never sentences.

Sizes are roles: `--text-eyebrow` 10px, `--text-xs` 10.5px (meta, keycaps), `--text-s`
11.5px (chrome labels, tabs, buttons), `--text-m` 13px (prose), `--text-l` 15px (surface
titles), `--text-xl` 22px (the one big numeral a stat tile is allowed). The terminal stays at
14px / 1.1 line-height, unchanged, because cell metrics are the one typographic knob that
moves a pane's geometry.

Windows 10 is the baseline. Segoe UI Variable is Windows 11-only and the Cascadia faces ship
with Windows Terminal and Visual Studio rather than with the OS — both chains fall through to
a face that is on every supported Windows (Segoe UI, Consolas). Neither chain may lose its
fallback.

## Shape and rhythm

Radius says whether a thing is an *object* or part of a *list*: `--radius-s` 6px for chips
and inputs, `--radius-m` 10px for panels and cards, `--radius-l` 14px for surfaces that
float, and `--radius-pill` for status chips. Rows, bars and pane frames stay square — they
tile, and a tiled thing with rounded corners reads as a mistake.

Spacing is a 4px rhythm (`--space-1..7`, 4/6/8/12/16/24/32) and it is where "density with
air" is actually spent: rows get vertical space, groups get `--space-6` between them, and
cards get `--space-7` inside.

## Layout — the frame

```
┌──────┬────────────────────────────────────────────────────────┐
│      │ ▤ loomux                          ⟲ sessions   ◫   ⬓   │
│ rail ├────────────────────────────────────────────────────────┤
│      │ tabs ················································· │
│ (see │ ┃ orchestrator        ⋯ │ ┃ w-386  ui/879-tokens    ⋯ │
│  X10)│ ┃                       │ ┃                           │
│      │ ┃     terminal          │ ┃     terminal              │
│      │ ┃                       │ ┃                           │
│      │ └ the warp: 2px, colour = live agent state             │
│      ├────────────────────────────────────────────────────────┤
│      │ CPU ▓▓░░  MEM ▓▓▓░  GPU ▓░░░  VRAM ▓▓░░           ? ⌘ │
└──────┴────────────────────────────────────────────────────────┘
```

- **The grid is the content** and takes every pixel it can.
- **Overlays are overlays.** Git view, task board, audit, issues, file manager and the
  session browser open *over* the grid and close again. No overlay becomes a split, and no
  overlay changes a pane's size: panes have exactly one geometry authority and it is the pane
  system (#885), not the chrome. This is `CLAUDE.md` constraint 1, and it is not a limitation
  to design around — it is what keeps scrollback intact.
- **One bar at the bottom, not two** (X9), and **a rail on the left if it earns its place**
  (X10). Both are experience changes, specified below rather than assumed here.

## The signature — the warp

One signature element, and the argument for keeping it through a change of visual language.

**The selvedge.** Every pane frame carries a 2px vertical thread down its left edge,
continuing through the pane header. Its colour is the pane's live agent state — azure
working, amber needs-you, jade done, rose failed, dashed faint mist held, a slate hairline
idle. Because panes tile side by side, the threads of adjacent panes stand parallel across
the whole grid: the fleet's state *is* a warp, read left to right, without a single badge.

**Why it survives the redirect.** The temptation, given "make it feel like ORCA", is to
reproduce the most distinctive object in the reference — its floating overview card with the
big stat numerals. That is precisely what the no-copy line forbids, and it is the right
prohibition: a borrowed signature is how you end up with someone else's identity again, which
is the mistake the Tokyo Night palette already made here. So loomux needs its own device *in
ORCA's language*, and the warp is native to loomux in a way it could not be to the reference:
ORCA lists workspaces in a rail, while loomux **tiles live terminals**, and a vertical state
edge only resolves into one picture if the things carrying it are tiled. It costs no colour
budget (it *is* the state colour), no space, and no geometry.

**If the rail ships (X10), the rail carries the aggregate view** — one thread per agent row,
in the same colours, so the rail and the grid read as the same warp at two scales. Until
then, a short strip of threads beside the brand mark in the top bar does that job. The strip
is a fallback, not a second signature; if the rail lands, the strip goes.

Three rules keep it safe and one keeps it honest:

- **The thread's width is constant** (`--thread: 2px`) and only its colour changes. A state
  change may never alter a size. It is the only weight in the app that is not the hairline
  (`--line-w: 1px`) — two weights, and no third.
- **It is drawn as a positioned pseudo-element, never as a border in the pane's box**, so the
  frame's geometry is untouched and the thread cannot cost a reflow or a ConPTY resize.
- **No blur or `backdrop-filter` near a pane.** The stylesheet has exactly one today — the
  cold-boot restore splash, shown before any pane exists, so it never composites over a WebGL
  canvas. It is the only one, and a slice that adds a second is wrong.
- **One animation exists in this app**: an attention thread pulses its opacity, slowly, and
  only while an agent is actually waiting on the human. It stops when acknowledged and does
  not run under `prefers-reduced-motion`. Everything else animates on state change only.

## The experience redesign

The human widened the ask past colour, so this section proposes the experience in the new
language. **None of it is built in slice A.** Each item is either mapped to a slice the plan
already has, or flagged **NEW-SLICE-NEEDED** for the orchestrator to take back to plan-380 —
the two flagged items change behaviour or geometry, which is not a restyle and must not be
smuggled into one.

| # | Change | Lands in |
| --- | --- | --- |
| X1 | The elevation model applied: every surface assigned a level, shadows only on floating ones | **B** |
| X2 | Radii, spacing rhythm and the eyebrow applied across the stylesheet | **B** |
| X3 | Tab bar quieted: status as a dot, close on hover, chips only where the word is the information | **C** |
| X4 | Pane header goes two-tier: agent name in sans, path and branch in dim mono, state moves to the thread | **D** |
| X5 | The dock reads as a row of status chips rather than mini-windows | **D** |
| X6 | Boards (task, audit, timeline) adopt eyebrow + card, losing most of their borders | **E** |
| X7 | A status-chip primitive in the shared kit — dot, label, one subtle fill — used by C, D, E, F | **G** |
| X8 | Welcome, restore and session surfaces become floating cards with a stat tile row | **F** |
| X9 | The two bottom bars collapse into one; keyboard hints become on-demand | **NEW-SLICE-NEEDED** |
| X10 | A left rail organising workspaces and the agent roster | **NEW-SLICE-NEEDED** |

### X9 — one bar at the bottom, not two

Today the app spends two full-width strips on chrome that never changes: `#hintbar` carries
**eleven** keyboard shortcuts plus a usage tip, and `#statusbar` carries four resource
meters. Stacked on top of
the top bar and the tab bar, that is four horizontal bands of chrome around the work. The
hints are valuable in week one and noise thereafter, and they are the band that earns the
least.

Proposal: one bottom bar carrying the resource meters, with the hints moved behind a `?`
affordance at its right edge — a quiet, permanent entry point to the full list, rather than
the list itself. This is **NEW-SLICE-NEEDED** because it removes a surface people currently
learn from: it needs a discoverability answer (does the overlay appear on first run? is there
a first-run state at all?), and that is a product decision, not a restyle.

### X10 — the rail, and whether loomux should have one

ORCA organises around a left rail: workspaces, then grouped lists of work with status chips.
It is the most legible thing about it, and the honest question is whether loomux's model
wants one.

**The case for.** loomux's fleet is currently legible only as tabs plus whatever panes happen
to be on screen; agents in other tabs, minimised, or docked are invisible until you go
looking. A rail listing every agent with its state — the same warp colours — would answer
"what is happening and where do I have to look" in one glance, which is principle 6.

**The case against, and it is real.** loomux already has a left panel, and it is a warning
rather than a precedent: `#sessions` is an in-flow flex sibling of the grid at `width: 344px`
with a `0.24s` width **transition**, so opening it shrinks the grid — resizing every pane,
and doing so on every frame of the animation. That is exactly the PTY-resize cost constraint
1 exists to prevent. A rail that toggles would repeat the mistake at higher frequency.

**Recommendation.** A rail, yes — but a **persistent, fixed-width** one that is part of the
app frame and never animates its width, so the geometry cost is paid once at startup like any
other version change. It should *replace* work the tab bar is doing rather than add a second
navigation system beside it. Both of those are structural decisions with geometry
consequences, which makes this **NEW-SLICE-NEEDED** and a coordination point with **#885**,
who owns everything that changes a pane's size. `--rail-w` is reserved in the token layer so
the decision has a single place to live; nothing consumes it yet.

## Self-critique — what was revised, and away from what

**Round 1 is the most useful thing in this section, because it failed.** The
`frontend-design` skill names three looks AI-generated design falls into; the app was in the
second (near-black, one bright accent), so the first proposal moved *away* from all three —
a warm undyed-flax ground, no chrome accent at all, textile vocabulary throughout. It was
internally coherent, it was not a default, and the human rejected it in one line: the
yellowish black was disliked, and what they wanted was the reference language they had named
in the issue from the start.

The lesson is not "be more generic". It is that **avoiding the defaults is not the goal;
serving the brief is** — and the skill says so explicitly: where the brief pins a direction,
the brief's own words win, *including* when they ask for one of the named defaults. Round 1
spent its originality on the axis the human had already fixed (the palette family) and had
none left for the axis they actually cared about (the experience). This revision inverts
that: the palette family is the reference's, and the originality is spent on structure — the
elevation model, the eyebrow, the state discipline, and a signature the reference cannot
have.

Three specific things were revised away:

**The warm ground is gone, entirely.** Not neutralised — inverted. `B−R` is positive at every
step of the slate ramp where it was negative at every step of the old one.

**The no-accent position was softened to one restrained accent.** Round 1 abolished the
chrome accent to make the state dyes mean something; the human asked for restrained accent
usage, not none, and a UI with no interactive colour at all makes "what can I click" a
guessing game. The scarcity argument is preserved a cheaper way: one accent, reused from the
state palette rather than added to it, with form separating the two meanings.

**The textile vocabulary is gone.** Round 1 named its palette fibre, linen, indigo, saffron,
verdigris and madder. In a cool grey cockpit those words would be a costume — and dressing a
motif in vocabulary rather than in structure is the exact failure round 1's own critique
identified in *its* first draft. The names are now plain (slate, mist, azure, amber, jade,
rose) and the loom idea survives only where it does structural work: the warp, which encodes
the tiling geometry, and nothing else.

**The nearest remaining risk, named honestly.** This palette is now inside the skill's
default look #2 — near-black with an accent — which is where the app started. What keeps it
from being the templated version of itself is not the hue: it is that the accent is rationed
to one meaning, that depth rather than colour carries hierarchy, that the four dyes are
reserved for agent state, and that the structure (eyebrows, chips, the warp) does the work
decoration usually does. Those are checkable claims, and the maintainability rules below are
what keep them true after slice B.

The accessory removed at the end: the top-bar thread strip, which becomes a fallback rather
than a second home for the signature the moment the rail exists.

## Maintainability rules — binding on slices B through J

1. **No raw colour outside the token block in `src/styles.css`.** Surfaces consume semantic
   tokens. Slice B migrates the literals that predate this rule and lists any documented
   exception in its PR.
2. **No new colour without a role.** A colour that is not one of the six names, in one of the
   declared roles, does not go in. "It needed a slightly different blue" is the failure mode
   the token layer exists to prevent.
3. **Colour is state; the accent is interaction.** Chrome takes slate and mist. If a surface
   wants a dye, the question is which agent state it is reporting; if the answer is "none",
   it does not get one.
4. **State changes colour, never size.** No hover, focus, or attention style may change a
   pane's box, `.xterm` padding, or terminal font metrics — those move cell geometry
   (`doc/design/xterm-resize-reflow.md`) and belong to #885.
5. **Shadows only on surfaces that float**, never under permanent chrome, and never a large
   soft shadow over a terminal. No blur or `backdrop-filter` beyond the one existing
   exception.
6. **`color-scheme: dark` stays on `:root`.** Dropping it regresses native widget colours
   silently.
7. **The three colour surfaces move together.** See the next section.

## Pinning — one palette, three languages

loomux's colours have to exist in three places that cannot read each other:

- `:root` in `src/styles.css`, which styles the chrome;
- the critical `<style>` block in `index.html`, which paints the app ground *before* the
  bundle exists (without it, startup flashes an unstyled white page);
- the xterm.js `ITheme`, because terminals render on a WebGL canvas that CSS custom
  properties never reach.

`src/theme.ts` is the one copy. It is DOM-free so `node --test` can import it directly, and
`test/theme.test.ts` reads the other two surfaces off disk and fails if either drifts: the
stylesheet must declare every pinned token with theme.ts's value, `index.html` must paint
exactly `PRE_PAINT_BACKGROUND`, and `pane.ts` must contain no colour literal at all.

The stylesheet pin runs **both ways**, which matters most on slice B: theme.ts → stylesheet
catches a token that drifts, and stylesheet → theme.ts catches a token *minted* in `:root`
with a literal value, which would be a fourth copy created by the very slice that is supposed
to be removing copies. A `var(...)` value is an alias onto something already pinned, so the
legacy bridge passes without exception; the one bridge declaration that is a literal
(`--accent-glow`) is named in the test and dies with the bridge. The guard covers
declarations whose *value is a colour* — a colour embedded inside a composite value, such as
the alpha-black in a shadow token, is not reached by it, which is why shadow tokens are
alpha-black only and are declared in this block where a reader can see all of them at once.

The sixteen ANSI slots are additionally checked present, pairwise distinct, and legible
against the terminal ground — sixteen near-identical hex strings is the exact shape a
copy-paste typo hides in, and a collapsed slot would make some CLI's output invisible with no
error anywhere.

Build-time codegen from CSS to TS was considered and rejected: it is a build step plus a
generated file to review, for one shared seam that a test and a comment already hold.

## What slice A shipped, and what it did not

Shipped: this note, `src/theme.ts`, the semantic token layer in `:root`, `TERM_THEME` derived
from `theme.ts`, the `index.html` pre-paint sync, and `test/theme.test.ts`.

Not shipped: any restyling — and the honest description of that is not "the old design
wearing the new colours", because only the rules that went through a *token* moved. The
eleven pre-redesign token names survive as an explicitly temporary **legacy bridge** aliasing
onto the new layer, so everything that consumed one is now on the new palette and nothing
breaks. Everything that hard-coded a colour instead is untouched: **387 colour literals** sit
below the token block (243 hex, 144 `rgb()`/`rgba()`), **165 of them the retired Tokyo Night
palette** this brief renounces — 57 of the old amber, 39 red, 33 blue, 21 green, 10 cyan, 5
magenta — concentrated in the task board, the audit log, the workflow pane and its mode
chrome, project tabs, session restore, and the attention badge. Until slice B those surfaces
stay visibly on the old palette while the rest moves, which is a transitional state, not the
design.

Slice B migrates the literals and deletes the bridge; slices C through I restyle the surfaces
to this brief; X9 and X10 need a plan decision before anyone starts them.

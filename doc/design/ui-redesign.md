# The loom — loomux's visual design brief (#879)

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

Everything in this brief is downstream of that. The chrome is not the product; the panes
are. So the chrome recedes, and the one thing it is allowed to be loud about is state.

## The research pass — principles, not pixels

The issue names two references: **ORCA** (onorca.dev — an Electron "agent development
environment" running parallel agents in worktrees) and **T3 Code**
(github.com/pingdotgg/t3code — an agent-harness control surface with desktop, web and
mobile clients). Both were read as marketing pages, docs and public discussion. Five
principles came out of it, and they are the only thing that came out of it:

1. **Chrome recedes; the work is the content.** Panels sit close together in luminance and
   separate with a hairline and with space, not with heavy contrast blocks. Active surfaces
   are marked by a luminance lift, not by a coloured slab.
2. **Density is respect.** A supervisor's screen should show many agents' states at once.
   Dense is not cramped: it needs a strict spacing rhythm to stay readable, and it earns its
   density by removing decoration, not by shrinking type.
3. **Data is set in mono.** Names, branches, ids, counts, timings — the chrome of an
   agent tool is almost entirely data, and data reads better, aligns better, and is
   *scanned* better in a monospace face.
4. **State is colour; colour is state.** Agent state is carried by small consistent colour
   signals rather than by boxy badge components. The corollary is the part most tools miss:
   if colour means state, then nothing that is *not* state may take colour.
5. **Supervision beats editing, and summaries come before logs.** The interesting question
   of the T3 Code design discussion is not "how do I edit this file" but "what is happening
   right now, and where do I have to look". Structure should answer that at a glance and let
   the human drill down after.

**The no-copy line — binding on every slice of #879.** These references are studied for
principles only. No palette value, logo, layout signature, icon set, or distinctive
component from ORCA, T3 Code, or any other product may be lifted, traced, or approximated.
Nothing in this document is derived from a reference's colours or components; the palette
below comes from the subject (see the next section), and the layout concept is a name for
the architecture loomux already has. If a slice finds itself matching a reference closely,
that is a defect to report, not a shortcut to take.

The same discipline applies to *themes*: the palette this replaces was Tokyo Night's, hex
for hex, in `TERM_THEME`. Borrowing a well-liked theme is how an app ends up with someone
else's identity and no argument for any of it.

## Where the design comes from

The product is named for a loom. Its brand mark, `▤`, is a woven swatch. Its whole idea is
many threads held in parallel under one frame, under tension, watched by one person who
steps in when one of them snags. That is not a metaphor bolted on afterwards — it is a
literal description of what the software does, and it is where this design gets its
material: undyed fibre, dyed thread, the selvedge that keeps cloth from fraying, the beam
that gathers the warp.

Dye is the operative idea. Dye is expensive, it is deliberate, and you can tell at a glance
which threads have it. That maps exactly onto principle 4.

## The palette — six named threads

Dark-only, and staying that way: `color-scheme: dark` on `:root` keeps native widgets
(select popups, scrollbars) dark, and a light theme would double every demo surface for a
product whose users work at night against terminals. The tokens make one *possible* later;
this line does not build one.

| Thread | Value | Role |
| --- | --- | --- |
| **fibre** | `#0f0e0b` → `#3b352a` | the undyed ground: every surface, every border |
| **linen** | `#e4dfd2` / `#a49d8b` / `#767162` | the ink: primary, secondary, faint |
| **indigo** | `#7687d6` | agent **working** — in flight, information |
| **saffron** | `#e2a33c` | **the human** — needs-you, focus, the caret |
| **verdigris** | `#4fae94` | **ok** — done, passing, additions |
| **madder** | `#dd665e` | **danger** — failed, error, deletions |

Two things about this are choices rather than defaults, and both are worth defending.

**The ground is warm, not blue-black.** `fibre` runs from `#0f0e0b` to `#3b352a`: red a few
points above green above blue, at roughly 3% chroma. It is the colour of unbleached flax in
a dark room. Every agent tool, this one included until now, uses a blue-black or a neutral
graphite; a warm near-black is immediately identifiable, it makes the ANSI blues and cyans
in terminal output pop rather than sink, and at 6–14% lightness it reads as *warm*, not as
brown and not as an amber CRT. The four **surface** steps are tiny on purpose — 1.047,
1.060 and 1.083:1 between neighbours — because principle 1 says panels separate with a
hairline, not a slab; `test/theme.test.ts` fails a surface step above 1.3:1. The two
**border** steps above them deliberately open up (1.123:1 and 1.177:1), because an edge
that nobody can see is not doing the separating the surfaces are refusing to do.

**Chrome carries no hue at all.** There is no "accent colour" in this design. Buttons,
tabs, borders, headers, and the active-pane indicator are fibre and linen; the four dyes are
reserved entirely for agent state and for the human's own position. This is principle 4
taken to its conclusion, and it is the single biggest departure from the UI it replaces
(which spent a blue accent on every button, tab, brand mark and focus ring, and then had
nothing distinctive left to say "this agent needs you" with). Scarcity is what makes colour
legible. If everything is accented, nothing is.

Two consequences follow, and both are rules:

- **Focus is luminance and ink, not colour.** The active pane lifts a surface step and
  brightens its header ink; inactive panes dim. A `:focus-visible` ring is saffron, because
  a keyboard ring is exactly "where the human is" and must be unmistakable — but that is a
  ring at the point of interaction, never a glow around a panel.
- **Stopped agents get no dye.** `held` is faint linen and `idle` is a fibre hairline. A
  held agent is not running, so it has no dye; it is marked by *form* — a dashed thread —
  not by hue. This also keeps the state set legible for red/green colour blindness, because
  the two states that share a hue family (ok/danger) are never the two states a supervisor
  has to tell apart urgently; needs-you is saffron and stands alone.

The terminal needs eight hues where the app needs four, so `theme.ts` carries two
terminal-only dyes — **murex** (`#b57ec9`, ANSI magenta) and two pulls of verdigris toward
green (`#5fae7c`) and toward blue (`#45b3b8`) so ANSI green reads as green and ANSI cyan as
cyan. These are not app tokens. No UI surface may use them.

**Contrast is measured, not claimed.** `test/theme.test.ts` computes WCAG ratios over these
values on every run: primary ink clears AAA (7:1) on all four grounds, dim ink clears AA,
each of the four dyes clears AA (4.5:1) on every surface it can appear on, and faint linen
is *held* between 3:1 and 4.5:1 — deliberately below AA, because its role is non-essential
meta and rules. If a future edit makes faint ink readable, it has stopped being a separate
role and the test says so.

## Type — the chrome is woven from the terminal's own thread

Two roles, both system-resident. **No webfont is vendored, and that is an argument, not a
default.**

- `--font-mono` — `"Cascadia Code", "Cascadia Mono", Consolas, "Courier New", monospace`.
  Everything that is **data**: pane titles, branch names, agent ids, counts, timings, model
  names, keycaps, table cells, badges.
- `--font-ui` — `"Segoe UI Variable Text", "Segoe UI", system-ui, sans-serif`. **Prose
  only**: dialog body copy, empty-state text, tooltips, descriptions.

The obvious move for "make it look designed" is to vendor a characterful OFL face. This
brief refuses, for a specific reason: a webfont would give the chrome a *different voice
from the terminal*, and the seam between chrome and content is exactly the seam this design
wants to erase. Ninety-five percent of loomux's pixels are xterm output in Cascadia Code.
Setting the chrome's data in the same face makes the app read as one continuous cloth
instead of a sans-serif frame around a monospace picture. That is a stronger identity than
any display face would have bought, and it costs zero assets, zero licence files, and zero
flash-of-unstyled-text in a window whose first paint happens before the bundle exists.

The character comes from the *treatment*, not the family: mono at chrome sizes with
ligatures off (`font-variant-ligatures: none` — a `->` in a branch name is two characters,
not an arrow), tight tracking, and small caps-height labels. This is the one real aesthetic
risk in the brief. Monospace chrome is unusual and it can read as cramped or as a toy
terminal-emulator theme if it is executed badly; the mitigations are the size scale below
and the spacing rhythm, and the human demo is where it gets judged.

Windows 10 is the baseline. Segoe UI Variable is Windows 11-only and the Cascadia faces
ship with Windows Terminal and Visual Studio rather than with the OS — both chains fall
through to a face that is on every supported Windows (Segoe UI, Consolas). Neither chain may
lose its fallback.

Sizes are roles: `--text-xs` 10.5px (meta, keycaps), `--text-s` 11.5px (chrome labels,
tabs, buttons), `--text-m` 13px (prose), `--text-l` 15px (surface titles). The terminal
stays at 14px / 1.1 line-height, unchanged, because cell metrics are the one typographic
knob that moves a pane's geometry.

## Layout — the loom

The concept is a name for the architecture loomux already has, which is the point: the
constraint that overlays never resize a pane (`CLAUDE.md` #1) is not a limitation to design
around, it is the loom's frame.

```
┌───────────────────────────────────────────────────────────────┐
│ ▤ loomux  ‖‖│┊‖                              ⟲ sessions ◫ ⬓  │  the beam
├───────────────────────────────────────────────────────────────┤
│ project tabs ················································ │
│ ┃ orchestrator      ⋯ │ ┃ w-386  ui/879-tokens            ⋯ │
│ ┃                      │ ┃                                   │
│ ┃    terminal          │ ┃    terminal                       │  the web
│ ┃                      │ ┃                                   │
│ ┃                      │ ┃                                   │
│ └ selvedge: 2px, colour = live agent state                    │
├───────────────────────────────────────────────────────────────┤
│ Ctrl+Shift+E split right ····································· │
│ CPU ▓▓░░ MEM ▓▓▓░ GPU ▓░░░ VRAM ▓▓░░                          │  the take-up
└───────────────────────────────────────────────────────────────┘
```

- **The beam** (top bar) is fixed, quiet, and gathers the warp.
- **The web** (the pane grid) is the content and takes every pixel it can.
- **The take-up** (hint bar, status bar) is metering: faint linen, mono, never competing.
- **Everything else arrives as a shed** — git view, task board, audit, issues, file manager,
  session browser — a panel that opens *over* the web from one edge and closes again. No
  overlay ever becomes a split, and no overlay ever changes a pane's size. Panes have exactly
  one geometry authority and it is the pane system (#885), not the chrome.

## The signature — the warp

One signature element, two scales, the same picture at both:

**The selvedge.** Every pane frame carries a 2px vertical thread down its left edge,
continuing through the pane header. Its colour is the pane's live agent state — indigo
working, saffron needs-you, verdigris done, madder failed, dashed faint linen held, a fibre
hairline idle. Because panes tile side by side, the threads of adjacent panes stand parallel
across the whole grid: the fleet's state *is* a warp, read left to right, without a single
badge.

**The beam.** The top bar carries those same threads gathered into a short strip beside the
brand mark — one 2px tick per live pane, in pane order, in state colour. It is the fleet at
a glance when panes are minimised, docked, on another tab, or off-screen, and it doubles as
the mark that makes a loomux screenshot recognisable at thumbnail size. A tick focuses its
pane; that is the whole interaction.

This is where the design spends its boldness, and everything else stays quiet so that it
can. Three rules make it safe and one makes it honest:

- **The thread's width is constant** (`--thread: 2px`) and only its colour changes. A state
  change may never alter a size. It is the *only* weight in the app that is not the hairline
  (`--line-w: 1px`, the token every surface separates with) — two weights, and no third.
- **It is drawn as a positioned pseudo-element, never as a border in the pane's box.** The
  frame's geometry is untouched, so the thread cannot cost a reflow or a ConPTY resize —
  the seam with #885 is a hard one and this stays on the chrome side of it.
- **No blur, no `backdrop-filter`, no large filter anywhere near a pane.** Terminals render
  on WebGL canvases; compositing effects over them is the documented way to make this app
  slow (`doc/design/performance.md`). The token layer ships no blur token, deliberately.
- **One animation exists in this app**: an attention thread pulses its opacity, slowly, and
  only while an agent is actually waiting on the human. It stops when acknowledged and it
  does not run at all under `prefers-reduced-motion`. Everything else animates on state
  change only, at `--dur-fast` / `--dur-base`.

Slices C and D implement the two halves and share one class between them.

## Self-critique — what was revised, and away from what

The `frontend-design` skill names three looks that AI-generated design falls into, and
this app was squarely inside the second: a near-black ground with one bright accent. Two
rounds of revision moved off it.

**The first draft was the generic answer with better names.** It kept a single blue accent —
a nicer blue, a loom word attached to it — and put a coloured status bar across the top of
each pane header. Working through it, both halves were what any similar prompt produces: a
horizontal coloured bar on the top edge of a card is the most templated status device in
software, and "dark UI plus one blue accent" is precisely the default the skill warns about
and precisely what was already on screen. The loom vocabulary was doing decorative work
rather than structural work — the sign that a motif is a costume.

Three things changed. **The accent was removed rather than replaced**: the chrome now
carries no hue at all, which is a real position with real consequences (focus had to become
luminance, and stopped states had to become form), and it makes the four dyes mean
something. **The thread went vertical.** A left-edge selvedge is not just a rarer device
than a top-edge bar; it is the *correct* one, because panes tile horizontally and vertical
threads on adjacent panes line up into a warp that reads as one picture, while horizontal
bars would never align into anything. The device now encodes something true about the
content, which is what a structural device is supposed to do. **The ground was warmed off
blue-black**, which is the fastest way to stop looking like every other agent tool and is
grounded in the subject rather than in a mood board.

The nearest remaining risk is honest to name: a warm dark with earth-named colours is
adjacent to the skill's *first* default look (cream, serif, terracotta). The distance is
deliberate and specific — no cream, no serif anywhere, no terracotta, chroma held near 3% at
6–14% lightness so the warmth reads as material rather than as a palette, and every earth
tone is a *named dye with a job*, never a decorative fill. The second risk is monospace
chrome, argued above and left standing as the one risk this brief takes.

The one accessory removed at the end: an eighth palette entry (a separate chrome edge grey)
that sat 1.2:1 from faint linen and did the same job. ANSI bright-black is now the faint ink,
which is what bright-black actually means.

## Maintainability rules — binding on slices B through J

1. **No raw hex outside the token block in `src/styles.css`.** Surfaces consume semantic
   tokens. Slice B migrates the ~280 literals that predate this rule and lists any
   documented exception in its PR.
2. **No new colour without a role.** A colour that is not one of the six threads, in one of
   the declared roles, does not go in. "It needed a slightly different blue" is the failure
   mode the token layer exists to prevent.
3. **Colour is state.** Chrome takes fibre and linen. If a surface wants a dye, the question
   is which agent state it is reporting; if the answer is "none", it does not get one.
4. **State changes colour, never size.** No hover, focus, or attention style may change a
   pane's box, `.xterm` padding, or terminal font metrics — those move cell geometry
   (`doc/design/xterm-resize-reflow.md`) and belong to #885.
5. **No blur or filter composited over a terminal.** The stylesheet has exactly one
   `backdrop-filter` today — the cold-boot restore splash — and it is allowed to stay
   because it is shown before any tab or pane exists, so it never composites over a
   WebGL canvas. It is the only one, and a slice that adds a second is wrong.
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
with a literal value, which would be a fourth copy created by the very slice that is
supposed to be removing copies. A `var(...)` value is an alias onto something already
pinned, so the legacy bridge passes without exception; the one bridge declaration that is a
literal (`--accent-glow`) is named in the test and dies with the bridge. The
sixteen ANSI slots are additionally checked present, pairwise distinct, and legible against
the terminal ground — sixteen near-identical hex strings is the exact shape a copy-paste
typo hides in, and a collapsed slot would make some CLI's output invisible with no error
anywhere.

Build-time codegen from CSS to TS was considered and rejected: it is a build step plus a
generated file to review, for one shared seam that a test and a comment already hold.

## What slice A shipped, and what it did not

Shipped: this note, `src/theme.ts`, the semantic token layer in `:root`, `TERM_THEME`
derived from `theme.ts`, the `index.html` pre-paint sync, and `test/theme.test.ts`.

Not shipped: any restyling — and the honest description of that is not "the old design
wearing the new colours", because only the rules that went through a *token* moved. The
twelve pre-redesign token names survive as an explicitly temporary **legacy bridge**
aliasing onto the new layer, so everything that consumed one is now on the new palette and
nothing breaks. Everything that hard-coded a colour instead is untouched: **387 colour
literals** sit below the token block (243 hex, 144 `rgb()`/`rgba()`), **165 of them the
retired Tokyo Night palette** this brief renounces — 57 of the old amber, 39 red, 33 blue,
21 green, 10 cyan, 5 magenta — concentrated in the task board, the audit log, the workflow
pane and its mode chrome, project tabs, session restore, and the attention badge. Until
slice B those surfaces stay visibly on the old palette while the rest moves, which is a
transitional state, not the design. Slice B migrates the literals and deletes the bridge;
slices C through I restyle the surfaces to this brief.

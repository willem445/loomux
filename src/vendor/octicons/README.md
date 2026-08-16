# Primer Octicons — vendored agent-mark artwork (MIT)

`src/agenticons.ts` carries the glyphs loomux draws to say *which agent CLI is running in
this pane* (#992). This directory holds the licence those glyphs ship under and the
provenance for re-vendoring them. **Nothing here is imported by the bundle** — the artwork
lives in `src/agenticons.ts`; these are the papers that go with it.

## The pin

| | |
| --- | --- |
| Upstream | https://github.com/primer/octicons |
| Version | **19.33.0** |
| Commit | `cc4e12df6ff8292447ba9141eaa2a6f6e1c59a85` |
| Licence | MIT — full text in `LICENSE`, copied from the same commit |

The same commit hash appears in `src/agenticons.ts` (`OCTICONS_PIN`) and in
`THIRD_PARTY_NOTICES.md`. `test/agenticons.test.ts` fails if the three disagree.

## The vendored set

One glyph:

- `copilot-16` — GitHub Copilot

Each entry is the **inner markup** of the upstream `icons/<name>.svg` at the pinned commit,
verbatim. The paths carry no `fill` of their own, so `src/agenticons.ts` builds a wrapper
with `fill="currentColor"` and the glyph takes the surrounding ink — the same single-colour
rendering upstream ships.

## Two permissions, not one

MIT settles **copyright** in the artwork: GitHub grants the right to copy, redistribute and
ship it, on the condition that the notice above travels with it, which this file and
`THIRD_PARTY_NOTICES.md` discharge.

It does not, and no OSS licence does, grant anything in the **trademark**. That permission
is not needed here and is not claimed: loomux draws the Copilot mark on a pane *that is
running GitHub Copilot*, which is nominative use — the mark identifying the thing it names.
Two constraints follow, and they are the reason the vendoring rules above are strict:

- **The artwork is not modified.** Paths verbatim, upstream's own `viewBox`, no redraw and
  no recomposition. Recolouring to a single ink is what the octicon already does.
- **No affiliation is implied.** The mark labels a process the user launched; loomux's own
  branding is elsewhere, and nothing in the UI presents Copilot as a loomux product.

Octicons' own README points logo use at [GitHub's logo guidelines](https://github.com/logos);
the two constraints above are what those guidelines ask for.

## Why so little is vendored

Because the second tier does the work. Everything with no licensed glyph — Claude and
opencode today — renders as a generated letter badge, and that is a deliberate refusal
rather than a gap: a hand-traced lookalike is a derivative of a mark nobody granted, and a
third-party icon aggregator's CC0 covers the aggregator's tracing, not the trademark it
traces. See `§Licensing` in `src/agenticons.ts`.

**Only glyphs a surface actually renders may be vendored** — an unused entry is a licence
obligation buying nothing.

## Re-vendoring

1. Pick the release and resolve its commit
   (`gh api repos/primer/octicons/git/ref/tags/v<version>`).
2. For each name in the vendored set, take the **inner markup** of `icons/<name>.svg` at
   that commit, verbatim. Do not touch the path data and do not restyle.
3. Refresh `LICENSE` from the same commit and update the pin in all three places above.
4. `npm test` — `test/agenticons.test.ts` covers the shape of every entry, the fleet-role
   dye, the letter clamp and the provenance pin.

Adding a *new* CLI's mark is not a re-vendor: it is a licence decision first. Establish that
the vendor grants copyright over its own glyph before a path ever lands in the table, and
record which grant it was here.

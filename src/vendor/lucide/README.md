# Lucide — vendored icon artwork (ISC)

`src/icons.ts` carries a hand-picked set of [Lucide](https://github.com/lucide-icons/lucide)
glyphs as inline SVG string constants. This directory holds the licence those constants ship
under and the provenance for re-vendoring them. **Nothing here is imported by the bundle** —
the artwork itself lives in `src/icons.ts`; these are the papers that go with it.

## The pin

| | |
| --- | --- |
| Upstream | https://github.com/lucide-icons/lucide |
| Version | **1.31.0** |
| Commit | `b7b6ecf1316d0af64c97a6b0392abe5e816a8e30` |
| Licence | ISC — full text in `LICENSE`, copied from the same commit |

The same commit hash appears in `src/icons.ts` (`LUCIDE_PIN`) and in
`THIRD_PARTY_NOTICES.md`. `test/icons.test.ts` fails if the three disagree: a re-vendor that
updates the code and forgets the notice is the exact failure that makes a licence file wrong
rather than merely stale.

## Two licences, not one

Lucide is ISC, **except** for the icons it inherited from
[Feather](https://github.com/feathericons/feather), which stay MIT (Copyright (c)
2013-present Cole Bemis). Both texts are in `LICENSE`, and Lucide lists the affected names
there. Two of them are in loomux's set:

- `arrow-up`
- `trash-2`

If a future batch adds another Feather-derived name, add it to that list here and to
`THIRD_PARTY_NOTICES.md`.

## The vendored set

Thirty-two glyphs, listed by `ICON_NAMES` in `src/icons.ts`. **Only icons a surface actually
renders may be vendored** — an unused entry is a licence obligation with no benefit, and
`test/icons.test.ts` scans `src/` and fails on one. That test is also what stops the set
drifting into "we may as well take the whole package", which is the point at which a
vendored copy stops being auditable and should have been a dependency instead.

## Re-vendoring

1. Pick the release and resolve its commit
   (`gh api repos/lucide-icons/lucide/git/ref/tags/<version>`).
2. For each name in `ICON_NAMES`, take the **inner markup** of
   `icons/<name>.svg` at that commit, verbatim, and collapse its line breaks. Do not touch
   the path data, and do not restyle: `src/icons.ts` builds the wrapper from Lucide's own
   `viewBox`, `stroke-width`, caps and joins, so the glyphs keep the geometry they were drawn
   for and the diff stays reviewable against upstream.
3. Refresh `LICENSE` from the same commit, update the pin in all three places above, and
   re-check the Feather list.
4. `npm test` — the registry tests cover the shape of every entry, the role table and the
   provenance pin.

A renamed or retired upstream icon shows up as a 404 in step 2, not as a silent substitution;
resolve it by picking a replacement name deliberately, never by keeping the old body under
the new name.

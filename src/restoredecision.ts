// Pure restore-vs-fresh-vs-ask decision (#194). DOM-free so the whole matrix is
// unit-tested (test/restoredecision.test.ts); the boot wiring that renders the
// restore splash and remembers the choice lives in main.ts (Phase 4).
//
// The rule is deliberately tiny: the remembered preference decides, EXCEPT that
// with nothing worth restoring we always go fresh (never prompt over an empty
// session, never claim to "restore" a blank state).

import type { RestorePref } from "./tabstore";

/** What boot should do: rebuild the saved session, start clean, or ask first. */
export type RestoreOutcome = "restore" | "fresh" | "prompt";

/** Decide the boot restore behavior.
 *
 *  @param pref        the remembered preference (first run is "ask").
 *  @param hasSnapshot whether there is prior persisted state worth restoring
 *                     (at least one tab, and — for a meaningful restore — a
 *                     captured layout somewhere). main.ts computes this from the
 *                     decoded tabstore.
 *
 *  With no snapshot the preference is irrelevant → "fresh". Otherwise "ask"
 *  prompts (the splash), and an explicit "restore"/"fresh" is honored silently. */
export function decideRestore(pref: RestorePref, hasSnapshot: boolean): RestoreOutcome {
  if (!hasSnapshot) return "fresh";
  switch (pref) {
    case "restore":
      return "restore";
    case "fresh":
      return "fresh";
    case "ask":
      return "prompt";
    default:
      // decodeTabs already coerces an unknown preference to "ask"; treat any
      // stray value the same way rather than trusting it silently.
      return "prompt";
  }
}

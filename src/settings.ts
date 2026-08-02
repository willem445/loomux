// Durable app-wide settings (#370) — the first of these, and deliberately
// config-file-only: there is no Settings/Preferences UI anywhere in loomux
// today (checked before adding this), and CLAUDE.md's "don't design for
// hypothetical future requirements" argues against building one just to host
// a single boolean. Persisted via the SAME mechanism tabstore.ts/uistate.rs
// already use for the tab set (atomic write, corrupt-quarantine on load) —
// a sibling `settings.json`, not a new storage layer. Pure encode/decode here
// (DOM-free, unit-tested, mirrors tabstore.ts); main.ts loads it once at boot
// via pty.ts's loadSettings/saveSettings wrappers and calls setSettings(),
// after which every reader (pane.ts's keydown handler, synchronously, on
// every keystroke) just calls getSettings().
//
// No hot-reload: editing settings.json by hand takes effect on the next
// launch, not immediately — there is no file watch on it (unlike tabs.json,
// which the app itself writes continuously; this file is user-edited, and a
// watch would mean reconciling a concurrent external edit against in-memory
// state for a single boolean, which is not worth the complexity yet).

export interface AppSettings {
  /** Plain Ctrl+V pastes into a terminal pane, in addition to the always-on
   *  Ctrl+Shift+V (#370). Defaults to true — most users expect Ctrl+V to
   *  paste and never touch vim/readline's use of the raw key. Set to false
   *  to get plain Ctrl+V back for vim's VISUAL BLOCK mode, readline's
   *  quoted-insert, or any other in-pane program that wants it; Ctrl+Shift+V
   *  still pastes either way. See doc/design/clipboard.md's #370 section for
   *  the full tradeoff — this mirrors the call `Alt+V` made the other way
   *  (#155, shortcuts.ts): don't unconditionally steal a key an in-pane
   *  program may need, offer the interception as a choice instead. */
  pasteOnPlainCtrlV: boolean;
  /** How long a VISIBLE but unfocused pane batches its PTY output before
   *  writing it to xterm, in ms (#720). The focused pane is never throttled.
   *  `0` turns throttling off entirely, restoring the pre-#720 behaviour where
   *  every visible pane rendered every frame.
   *
   *  This is a knob, not a preference, and it exists because the payoff is
   *  machine-dependent in a way loomux cannot measure for you: the saving is
   *  render passes, so it scales with how many panes are streaming at once and
   *  with how expensive rendering is on your GPU/renderer. Setting it to `0`
   *  and relaunching is the A/B — see doc/design/pane-render-throttle.md. */
  unfocusedRenderThrottleMs: number;
}

/** Upper bound on `unfocusedRenderThrottleMs`. A hand-edit of `10000` would
 *  make unfocused panes update once every ten seconds, which reads as a broken
 *  app rather than as a tuning choice; clamp instead of honouring it. */
export const MAX_RENDER_THROTTLE_MS = 1000;

export const DEFAULT_SETTINGS: AppSettings = {
  pasteOnPlainCtrlV: true,
  // Six 60 Hz frames. Long enough that a streaming unfocused pane renders ~6×
  // less often (xterm coalesces writes within a frame into one render pass and
  // nothing across frames), short enough that a human glancing across the grid
  // reads it as motion rather than as a stutter — 10 Hz is well clear of the
  // few-Hz range where intermittent motion starts looking stepped. This is the
  // single source of truth for the shipped window; panethrottle.ts holds the
  // policy that consumes it and no copy of the number.
  unfocusedRenderThrottleMs: 0, // RED HALF (#720): the pre-#720 policy
};

/** Serialize settings for `saveSettings`. Always writes every known key (no
 *  omit-if-default trimming, unlike tabstore's leaf fields) — this file is
 *  meant to be hand-read/edited, so an absent key would just be confusing;
 *  the exhaustive Copy&Paste-equivalent of tabstore's encodeTabs. */
export function encodeSettings(s: AppSettings): string {
  return JSON.stringify(
    {
      pasteOnPlainCtrlV: s.pasteOnPlainCtrlV,
      unfocusedRenderThrottleMs: s.unfocusedRenderThrottleMs,
    },
    null,
    2
  );
}

/** Parse persisted settings, tolerating anything malformed by returning null
 *  (the caller then boots with `DEFAULT_SETTINGS`) — same fail-safe shape as
 *  tabstore's `decodeTabs`. A missing/wrong-typed individual key falls back
 *  to that key's default rather than invalidating the whole file, so a
 *  hand-edit that only sets `pasteOnPlainCtrlV` (and nothing else, today)
 *  still works even after a future key is added. */
export function decodeSettings(raw: string | null): AppSettings | null {
  if (!raw) return null;
  let v: unknown;
  try {
    v = JSON.parse(raw);
  } catch {
    return null;
  }
  if (!v || typeof v !== "object") return null;
  const r = v as Record<string, unknown>;
  return {
    pasteOnPlainCtrlV:
      typeof r.pasteOnPlainCtrlV === "boolean" ? r.pasteOnPlainCtrlV : DEFAULT_SETTINGS.pasteOnPlainCtrlV,
    unfocusedRenderThrottleMs: throttleMs(r.unfocusedRenderThrottleMs),
  };
}

/** Per-key fallback for `unfocusedRenderThrottleMs`. A NEGATIVE value takes the
 *  default rather than being clamped to 0: `0` is a meaningful setting (throttling
 *  off) and `-1` is a typo, so silently reading a typo as "off" would quietly
 *  disable the feature and look like it never worked. Anything non-numeric,
 *  non-finite, or absent takes the default the same way every other key does. */
function throttleMs(v: unknown): number {
  if (typeof v !== "number" || !Number.isFinite(v) || v < 0) {
    return DEFAULT_SETTINGS.unfocusedRenderThrottleMs;
  }
  return Math.min(v, MAX_RENDER_THROTTLE_MS);
}

// ---------- in-memory singleton ----------
//
// The impure half: a module-level cell so pane.ts's keydown handler — deep in
// a hot path with no natural place to thread a settings object through — can
// read the current value SYNCHRONOUSLY. main.ts is the only writer, once, at
// boot. Defaults to DEFAULT_SETTINGS before the boot load resolves, which is
// the safe direction to default (pasteOnPlainCtrlV: true matches the pre-#370
// silent-failure fix's own behavior, not a surprise regression during the
// brief pre-load window).

let current: AppSettings = DEFAULT_SETTINGS;

export function getSettings(): AppSettings {
  return current;
}

export function setSettings(next: AppSettings): void {
  current = next;
}

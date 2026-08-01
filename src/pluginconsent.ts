// What a human is shown before a plugin's declared capabilities go live
// (#377) — the plain-language half of the install-time approval gate.
//
// DOM-free on purpose (CLAUDE.md's convention for frontend logic that needs
// tests): this module decides WHAT the consent prompt says and in WHAT ORDER,
// and `pluginpaneview.ts` does the DOM/modal wiring around it. The rule that
// actually gates anything lives in the backend (`plugingrants.rs` +
// `pluginbroker::validate_open_request`) — nothing here is a security check,
// and a bug in this file can only make the prompt less useful, never let a
// capability through. Tested in `test/pluginconsent.test.ts`.
//
// The capability set is the closed v1 enum (`doc/design/pane-plugins.md`'s
// "The v1 enum"), mirrored the same way `pluginbroker.ts`/`pluginpaneview.ts`
// already mirror it. Adding a row here is part of the reviewed contract change
// that adds a capability, never a lookup that quietly defaults.

/** One capability, as the consent prompt describes it. */
export interface CapabilityDescription {
  /** The manifest's own capability string, verbatim — the thing the human is
   *  consenting to, so it is always shown even when the prose below is more
   *  useful. */
  id: string;
  /** Plain language, no jargon: what this lets the plugin actually do. */
  detail: string;
  /** The prominence tier #377 asks for: `fs.read` and `metrics.system` read
   *  real data off this machine, so they are flagged and sorted first.
   *  `storage` writes only its own namespaced blob and `panel` is inert. */
  sensitive: boolean;
}

const CAPABILITIES: Record<string, Omit<CapabilityDescription, "id">> = {
  "fs.read": {
    detail: "Read files inside its own installed folder (nowhere else, and never write).",
    sensitive: true,
  },
  "metrics.system": {
    detail: "See this machine's CPU and memory use, and the names of running processes.",
    sensitive: true,
  },
  storage: {
    detail: "Save its own settings. No other plugin can read them.",
    sensitive: false,
  },
  panel: {
    detail: "Draw its own interface in the pane. Every plugin can do this.",
    sensitive: false,
  },
};

/** Describe one declared capability. An UNKNOWN string (a manifest from a
 *  newer loomux, or a hand-edited one) is described as unknown and treated as
 *  sensitive rather than quietly rendered as harmless — the honest answer to
 *  "what does this let it do" is "this build doesn't know", and that is
 *  exactly the case a human should look twice at. The backend refuses such a
 *  string outright, so this text is what a human sees on the way to that
 *  refusal, not a grant. */
export function describeCapability(capability: string): CapabilityDescription {
  const known = CAPABILITIES[capability];
  return known
    ? { id: capability, ...known }
    : {
        id: capability,
        detail: "Unrecognized by this version of loomux — it will be refused.",
        sensitive: true,
      };
}

/** Every declared capability, **most powerful first** and deduped: the
 *  prominence tier #377 asks for is an ORDER as much as a flag, so the row a
 *  human should think hardest about can't end up below two harmless ones.
 *  Ties keep a stable alphabetical order so the prompt doesn't reshuffle
 *  between openings of the same plugin. */
export function describeCapabilities(capabilities: readonly string[]): CapabilityDescription[] {
  return [...new Set(capabilities)]
    .map(describeCapability)
    .sort((a, b) => (a.sensitive === b.sensitive ? a.id.localeCompare(b.id) : a.sensitive ? -1 : 1));
}

/** The consent prompt's itemized body — one line per capability, in the order
 *  above, each leading with the capability's own manifest string so the human
 *  can match what they're reading against the manifest itself. Rendered with
 *  `textContent` per line by the caller (modal.ts's `bodyLines`), never as
 *  markup: the plugin's name/version are untrusted third-party text, and the
 *  capability strings are only trusted because they came back through the
 *  backend's closed-enum validation. */
export function consentLines(capabilities: readonly string[]): string[] {
  const described = describeCapabilities(capabilities);
  if (described.length === 0) {
    return ["No capabilities — it can draw in its pane and nothing else."];
  }
  return described.map((c) => `${c.sensitive ? "⚠ " : ""}${c.id} — ${c.detail}`);
}

/** Whether this set contains anything from the flagged tier — drives the one
 *  extra sentence the prompt shows above the list, so "this plugin only wants
 *  to save its own settings" doesn't read with the same weight as "this
 *  plugin wants to read files". */
export function hasSensitiveCapability(capabilities: readonly string[]): boolean {
  return capabilities.some((c) => describeCapability(c).sensitive);
}

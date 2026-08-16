// The icon registry, and the role→dye table that colours it (#879, slice K).
//
// WHY THIS MODULE EXISTS. loomux already drew its icons the right way — inline
// `currentColor` SVG strings in DOM-free TypeScript, no icon font, no sprite sheet, no
// dependency — but it drew them in four places (pane.ts, fileicons.ts, fileexplorer.ts,
// and ad-hoc glyphs in the views), each hand-authored to a different stroke weight and
// grid, and every one of them inheriting the surrounding text colour. That is a set of
// marks, not a system: nothing said which icons belong together, and nothing could say it,
// because they all came out the same grey.
//
// So this module owns two decisions, and they are separate on purpose:
//
//   1. WHAT THE MARK LOOKS LIKE — a vendored Lucide glyph (see §Vendoring), verbatim.
//   2. WHAT IT MEANS — a ROLE, and through the role an identity dye (see §The role table).
//
// COLOUR IS ASSIGNMENT, NOT ASSET. No icon body below carries a colour: they are
// `currentColor` line art, and the hue arrives from a CSS class this module stamps onto the
// `<svg>` element (`.ic-<role>` in styles.css, one `var(--id-*)` each). Recolouring the app
// therefore never touches an SVG, and a consumer cannot pick a hue — it picks a MEANING and
// the table picks the hue. That is the whole reason the role sits in the registry rather
// than in an argument at the call site: doc/design/ui-redesign.md's maintainability rule 3
// allows an `--id-*` token only "through a documented role mapping, never ad hoc", and a
// per-call override would be exactly the ad-hoc route it refuses.
//
// DOM-free on purpose: test/icons.test.ts imports this directly (no jsdom, no bundler), and
// the strings are injected with `innerHTML` by whoever needs them, exactly as before.

/**
 * §Vendoring — where the artwork comes from, and the rule for changing it.
 *
 * Lucide (ISC), vendored VERBATIM at the pin below: each body is the inner markup of the
 * upstream `icons/<name>.svg`, copied unmodified, with only its line breaks collapsed. The
 * wrapper this module builds carries Lucide's own `viewBox`, `stroke-width`, caps and joins
 * unchanged too, so the glyphs keep the geometry they were drawn for and a re-vendor is a
 * diff a reviewer can read against upstream.
 *
 * NOT an npm dependency, deliberately: the redesign plan's no-new-dependency line holds, the
 * copy is auditable in-tree, and nothing in the build has to resolve an icon package. The
 * cost is that a re-vendor is manual, which is what the pin and the provenance file are for
 * — `src/vendor/lucide/` carries the ISC licence text, the icon list and the procedure, and
 * THIRD_PARTY_NOTICES.md carries the shipped-in-repo entry. The commit below appears in all
 * three, and test/icons.test.ts fails if the three ever disagree.
 *
 * ONLY ICONS THAT ARE USED MAY BE VENDORED. A vendored asset nobody renders is a licence
 * obligation with no benefit, and the set grows silently if nothing checks; the test walks
 * `src/` and refuses an entry below that no surface asks for.
 */
export const LUCIDE_PIN = {
  repo: "https://github.com/lucide-icons/lucide",
  version: "1.31.0",
  commit: "b7b6ecf1316d0af64c97a6b0392abe5e816a8e30",
  license: "ISC",
} as const;

/**
 * §The role table — eight roles, eight hues, one claim each.
 *
 * A role answers the identity channel's question (doc/design/ui-redesign.md, §The three
 * colour channels): *which thing is this?* — never *what state is it in*. That is why no
 * role below names a `--state-*` token or the accent: an icon that reports agent state
 * takes its colour from the POSITION it sits in (the warp thread, the status chip, the state
 * dot), which belongs to the surfaces C–I paint, not to this registry. Keeping the two
 * apart here is what lets the app get much more colourful without diluting the four signals
 * a supervisor has to read across ten panes.
 *
 * THE BIJECTION IS THE DISCIPLINE. Each of the eight identity hues is claimed by exactly one
 * role, so a hue IN AN ICON resolves to one meaning rather than to "one of the two or three
 * families that happen to be amber". Scoped to icons on purpose, and the scope is not a
 * hedge: the same four hues are claimed again by the agent-role table (azure/jade/violet/
 * amber = orchestrator/worker/reviewer/planner, pinned by test/theme.test.ts), so a hue is
 * one meaning per surface, not one meaning app-wide. docs/core-concepts.md says the same
 * thing to users. It is also what stops the table from growing: a ninth
 * family cannot be added by minting a ninth colour (the brief measured eight as the ceiling)
 * — it has to argue its way into an existing role or displace one. test/icons.test.ts
 * enforces the bijection in both directions.
 *
 * The per-CLI hues (theme.ts §CLI_HUES, `--cli-*`) are the one set that ever got past that
 * ceiling, and they got past it by NOT joining this table: a CLI could not take a role's hue
 * without giving that hue a second meaning, which is the bijection's whole point, so it has
 * its own tokens and its own pin (test/agenticons.test.ts). Nothing below changes — the roles
 * are still eight, still bijective, and a `.cli-*` rule never meets an `.ic-*` one on the same
 * element.
 */
export type IconRole =
  /** Where the work lives: folders, paths, the file actions that move them around. */
  | "workspace"
  /** Code you edit — source files, and the two affordances that open an editor. */
  | "source"
  /** Data and documents you read: config, markup, images, lockfiles, plain text. */
  | "content"
  /** The repository's history: branches and the commit graph. */
  | "vcs"
  /** The agents themselves: the group, and the controls that act on every pane at once. */
  | "fleet"
  /** The group's work surfaces: tasks, issues, the audit log, the progress timeline. */
  | "board"
  /** Destructive actions. The one role whose hue agrees with its state twin, and it should. */
  | "danger"
  /** Capture in progress — the push-to-talk mic while it is listening. */
  | "live";

/**
 * Role → the identity token that dyes it. Values are CSS custom-property NAMES, not hexes:
 * the pigment lives in src/theme.ts and `:root`, and this module never learns one.
 *
 * Four of these hues are also state pigments (amber, jade, rose, azure). That duplication is
 * the brief's, not a slip — loomux has one palette and POSITION separates the channels — so
 * what matters is the token a surface NAMES, and every name below is `--id-*`.
 */
export const ROLE_TOKEN: Record<IconRole, string> = {
  workspace: "--id-cyan",
  source: "--id-amber",
  content: "--id-jade",
  vcs: "--id-lime",
  fleet: "--id-violet",
  board: "--id-orchid",
  danger: "--id-rose",
  live: "--id-azure",
};

/** Every vendored glyph, by its upstream Lucide name. */
export type IconName =
  | "folder"
  | "folder-open"
  | "folder-plus"
  | "file-plus"
  | "pencil"
  | "arrow-up"
  | "paperclip"
  | "file-code"
  | "file-box"
  | "file-play"
  | "file-terminal"
  | "globe"
  | "palette"
  | "code-xml"
  | "file-pen"
  | "file-braces"
  | "file-type"
  | "file-image"
  | "file-sliders"
  | "file-lock"
  | "file-text"
  | "file"
  | "git-branch"
  | "git-graph"
  | "users"
  | "chevrons-down-up"
  | "list-checks"
  | "circle-dot"
  | "chart-gantt"
  | "clock-fading"
  | "trash-2"
  | "mic";

/**
 * The mapping that makes the app colourful: every glyph declares its role once, here.
 *
 * A glyph has exactly ONE role even where it appears on several surfaces — `folder` is the
 * same cyan in a pane's cwd chip and in a file tree, because it is answering the same
 * question in both. If a mark ever genuinely needed two meanings it would need two entries,
 * and writing that down would be the point at which someone noticed.
 */
export const ICON_ROLE: Record<IconName, IconRole> = {
  // workspace — the tree's containers and the actions that reshape it.
  folder: "workspace",
  "folder-open": "workspace",
  "folder-plus": "workspace",
  "file-plus": "workspace",
  pencil: "workspace",
  "arrow-up": "workspace",
  paperclip: "workspace",

  // source — the languages loomux's users actually run agents over, plus the two ways in.
  // `globe` is markup and `palette` is stylesheets: both are authored, so both are source.
  "file-code": "source",
  "file-box": "source",
  "file-play": "source",
  "file-terminal": "source",
  globe: "source",
  palette: "source",
  "code-xml": "source",
  "file-pen": "source",

  // content — files you read rather than run. The generic `file` lives here because an
  // unclassified file is far more often data than code.
  "file-braces": "content",
  "file-type": "content",
  "file-image": "content",
  "file-sliders": "content",
  "file-lock": "content",
  "file-text": "content",
  file: "content",

  "git-branch": "vcs",
  "git-graph": "vcs",

  users: "fleet",
  "chevrons-down-up": "fleet",

  "list-checks": "board",
  "circle-dot": "board",
  "chart-gantt": "board",
  "clock-fading": "board",

  "trash-2": "danger",

  mic: "live",
};

/**
 * The vendored bodies — Lucide @ LUCIDE_PIN, unmodified.
 *
 * `palette`'s dots carry `fill="currentColor"`, which is upstream's and is kept: it is a
 * colour REFERENCE, not a colour, so it dyes with the rest of the glyph. The test that
 * refuses colour literals in this table is written to know the difference.
 */
const BODY: Record<IconName, string> = {
  folder: `<path d="M20 20a2 2 0 0 0 2-2V8a2 2 0 0 0-2-2h-7.9a2 2 0 0 1-1.69-.9L9.6 3.9A2 2 0 0 0 7.93 3H4a2 2 0 0 0-2 2v13a2 2 0 0 0 2 2Z" />`,
  "folder-open": `<path d="m6 14 1.5-2.9A2 2 0 0 1 9.24 10H20a2 2 0 0 1 1.94 2.5l-1.54 6a2 2 0 0 1-1.95 1.5H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h3.9a2 2 0 0 1 1.69.9l.81 1.2a2 2 0 0 0 1.67.9H18a2 2 0 0 1 2 2v2" />`,
  "folder-plus": `<path d="M12 10v6" /><path d="M9 13h6" /><path d="M20 20a2 2 0 0 0 2-2V8a2 2 0 0 0-2-2h-7.9a2 2 0 0 1-1.69-.9L9.6 3.9A2 2 0 0 0 7.93 3H4a2 2 0 0 0-2 2v13a2 2 0 0 0 2 2Z" />`,
  "file-plus": `<path d="M6 22a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h8a2.4 2.4 0 0 1 1.704.706l3.588 3.588A2.4 2.4 0 0 1 20 8v12a2 2 0 0 1-2 2z" /><path d="M14 2v5a1 1 0 0 0 1 1h5" /><path d="M9 15h6" /><path d="M12 18v-6" />`,
  pencil: `<path d="M21.174 6.812a1 1 0 0 0-3.986-3.987L3.842 16.174a2 2 0 0 0-.5.83l-1.321 4.352a.5.5 0 0 0 .623.622l4.353-1.32a2 2 0 0 0 .83-.497z" /><path d="m15 5 4 4" />`,
  "arrow-up": `<path d="m5 12 7-7 7 7" /><path d="M12 19V5" />`,
  paperclip: `<path d="m16 6-8.414 8.586a2 2 0 0 0 2.829 2.829l8.414-8.586a4 4 0 1 0-5.657-5.657l-8.379 8.551a6 6 0 1 0 8.485 8.485l8.379-8.551" />`,
  "file-code": `<path d="M6 22a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h8a2.4 2.4 0 0 1 1.704.706l3.588 3.588A2.4 2.4 0 0 1 20 8v12a2 2 0 0 1-2 2z" /><path d="M14 2v5a1 1 0 0 0 1 1h5" /><path d="M10 12.5 8 15l2 2.5" /><path d="m14 12.5 2 2.5-2 2.5" />`,
  "file-box": `<path d="M14 2v5a1 1 0 001 1h5" /><path d="M14.692 22H18a2 2 0 002-2V8a2.4 2.4 0 00-.706-1.706l-3.588-3.588A2.4 2.4 0 0014 2H6a2 2 0 00-2 2v3.804" /><path d="M2.264 13.752 7 16.5l4.737-2.748" /><path d="M2.995 13.014A2 2 0 002 14.744v3.516a2 2 0 00.996 1.73l3 1.74a2 2 0 002.008 0l3-1.74A2 2 0 0012 18.26v-3.517a2 2 0 00-.995-1.73l-3-1.742a2 2 0 00-1.892-.064z" /><path d="M7 16.5V22" />`,
  "file-play": `<path d="M6 22a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h8a2.4 2.4 0 0 1 1.704.706l3.588 3.588A2.4 2.4 0 0 1 20 8v12a2 2 0 0 1-2 2z" /><path d="M14 2v5a1 1 0 0 0 1 1h5" /><path d="M15.033 13.44a.647.647 0 0 1 0 1.12l-4.065 2.352a.645.645 0 0 1-.968-.56v-4.704a.645.645 0 0 1 .967-.56z" />`,
  "file-terminal": `<path d="M6 22a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h8a2.4 2.4 0 0 1 1.704.706l3.588 3.588A2.4 2.4 0 0 1 20 8v12a2 2 0 0 1-2 2z" /><path d="M14 2v5a1 1 0 0 0 1 1h5" /><path d="m8 16 2-2-2-2" /><path d="M12 18h4" />`,
  globe: `<circle cx="12" cy="12" r="10" /><path d="M12 2a14.5 14.5 0 0 0 0 20 14.5 14.5 0 0 0 0-20" /><path d="M2 12h20" />`,
  palette: `<path d="M12 22a1 1 0 0 1 0-20 10 9 0 0 1 10 9 5 5 0 0 1-5 5h-2.25a1.75 1.75 0 0 0-1.4 2.8l.3.4a1.75 1.75 0 0 1-1.4 2.8z" /><circle cx="13.5" cy="6.5" r=".5" fill="currentColor" /><circle cx="17.5" cy="10.5" r=".5" fill="currentColor" /><circle cx="6.5" cy="12.5" r=".5" fill="currentColor" /><circle cx="8.5" cy="7.5" r=".5" fill="currentColor" />`,
  "code-xml": `<path d="m18 16 4-4-4-4" /><path d="m6 8-4 4 4 4" /><path d="m14.5 4-5 16" />`,
  "file-pen": `<path d="M12.659 22H18a2 2 0 0 0 2-2V8a2.4 2.4 0 0 0-.706-1.706l-3.588-3.588A2.4 2.4 0 0 0 14 2H6a2 2 0 0 0-2 2v9.34" /><path d="M14 2v5a1 1 0 0 0 1 1h5" /><path d="M10.378 12.622a1 1 0 0 1 3 3.003L8.36 20.637a2 2 0 0 1-.854.506l-2.867.837a.5.5 0 0 1-.62-.62l.836-2.869a2 2 0 0 1 .506-.853z" />`,
  "file-braces": `<path d="M6 22a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h8a2.4 2.4 0 0 1 1.704.706l3.588 3.588A2.4 2.4 0 0 1 20 8v12a2 2 0 0 1-2 2z" /><path d="M14 2v5a1 1 0 0 0 1 1h5" /><path d="M10 12a1 1 0 0 0-1 1v1a1 1 0 0 1-1 1 1 1 0 0 1 1 1v1a1 1 0 0 0 1 1" /><path d="M14 18a1 1 0 0 0 1-1v-1a1 1 0 0 1 1-1 1 1 0 0 1-1-1v-1a1 1 0 0 0-1-1" />`,
  "file-type": `<path d="M6 22a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h8a2.4 2.4 0 0 1 1.704.706l3.588 3.588A2.4 2.4 0 0 1 20 8v12a2 2 0 0 1-2 2z" /><path d="M14 2v5a1 1 0 0 0 1 1h5" /><path d="M11 18h2" /><path d="M12 12v6" /><path d="M9 13v-.5a.5.5 0 0 1 .5-.5h5a.5.5 0 0 1 .5.5v.5" />`,
  "file-image": `<path d="M6 22a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h8a2.4 2.4 0 0 1 1.704.706l3.588 3.588A2.4 2.4 0 0 1 20 8v12a2 2 0 0 1-2 2z" /><path d="M14 2v5a1 1 0 0 0 1 1h5" /><circle cx="10" cy="12" r="2" /><path d="m20 17-1.296-1.296a2.41 2.41 0 0 0-3.408 0L9 22" />`,
  "file-sliders": `<path d="M6 22a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h8a2.4 2.4 0 0 1 1.704.706l3.588 3.588A2.4 2.4 0 0 1 20 8v12a2 2 0 0 1-2 2z" /><path d="M14 2v5a1 1 0 0 0 1 1h5" /><path d="M8 12h8" /><path d="M10 11v2" /><path d="M8 17h8" /><path d="M14 16v2" />`,
  "file-lock": `<path d="M4 9.8V4a2 2 0 0 1 2-2h8a2.4 2.4 0 0 1 1.706.706l3.588 3.588A2.4 2.4 0 0 1 20 8v12a2 2 0 0 1-2 2h-3" /><path d="M14 2v5a1 1 0 0 0 1 1h5" /><path d="M9 17v-2a2 2 0 0 0-4 0v2" /><rect width="8" height="5" x="3" y="17" rx="1" />`,
  "file-text": `<path d="M6 22a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h8a2.4 2.4 0 0 1 1.704.706l3.588 3.588A2.4 2.4 0 0 1 20 8v12a2 2 0 0 1-2 2z" /><path d="M14 2v5a1 1 0 0 0 1 1h5" /><path d="M10 9H8" /><path d="M16 13H8" /><path d="M16 17H8" />`,
  file: `<path d="M6 22a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h8a2.4 2.4 0 0 1 1.704.706l3.588 3.588A2.4 2.4 0 0 1 20 8v12a2 2 0 0 1-2 2z" /><path d="M14 2v5a1 1 0 0 0 1 1h5" />`,
  "git-branch": `<path d="M15 6a9 9 0 0 0-9 9V3" /><circle cx="18" cy="6" r="3" /><circle cx="6" cy="18" r="3" />`,
  "git-graph": `<circle cx="5" cy="6" r="3" /><path d="M5 9v6" /><circle cx="5" cy="18" r="3" /><path d="M12 3v18" /><circle cx="19" cy="6" r="3" /><path d="M16 15.7A9 9 0 0 0 19 9" />`,
  users: `<path d="M16 21v-2a4 4 0 0 0-4-4H6a4 4 0 0 0-4 4v2" /><path d="M16 3.128a4 4 0 0 1 0 7.744" /><path d="M22 21v-2a4 4 0 0 0-3-3.87" /><circle cx="9" cy="7" r="4" />`,
  "chevrons-down-up": `<path d="m7 20 5-5 5 5" /><path d="m7 4 5 5 5-5" />`,
  "list-checks": `<path d="M13 5h8" /><path d="M13 12h8" /><path d="M13 19h8" /><path d="m3 17 2 2 4-4" /><path d="m3 7 2 2 4-4" />`,
  "circle-dot": `<circle cx="12" cy="12" r="10" /><circle cx="12" cy="12" r="1" />`,
  "chart-gantt": `<path d="M10 6h8" /><path d="M12 16h6" /><path d="M3 3v16a2 2 0 0 0 2 2h16" /><path d="M8 11h7" />`,
  "clock-fading": `<path d="M12 2a10 10 0 0 1 7.38 16.75" /><path d="M12 6v6l4 2" /><path d="M2.5 8.875a10 10 0 0 0-.5 3" /><path d="M2.83 16a10 10 0 0 0 2.43 3.4" /><path d="M4.636 5.235a10 10 0 0 1 .891-.857" /><path d="M8.644 21.42a10 10 0 0 0 7.631-.38" />`,
  "trash-2": `<path d="M10 11v6" /><path d="M14 11v6" /><path d="M19 6v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6" /><path d="M3 6h18" /><path d="M8 6V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2" />`,
  mic: `<path d="M12 19v3" /><path d="M19 10v2a7 7 0 0 1-14 0v-2" /><rect x="9" y="2" width="6" height="13" rx="3" />`,
};

/** Every vendored name, for tests and for anything that wants to walk the set. */
export const ICON_NAMES = Object.keys(BODY) as IconName[];

/** Lucide's grid. Uniform across the registry — a glyph on a different one would sit at a
 *  different optical weight beside its neighbours, which is the failure the old hand-drawn
 *  set had. */
export const ICON_VIEWBOX = "0 0 24 24";

/**
 * Render an icon as an inline SVG string, dyed by its role.
 *
 * `size` is the rendered box in px and defaults to the 14px the file trees use. It exists
 * only so the migrated call sites keep the exact box their hand-drawn glyph had (12 in a
 * pane's meta chips, 13 on the toolbar buttons) — this slice colours icons, it does not move
 * anything. Lucide's stroke-width of 2 on a 24 grid lands at ~1.1px at these sizes, which is
 * where the old 16-grid glyphs already were.
 *
 * `aria-hidden`: the mark is decorative in every consumer — buttons carry a `title`, tree
 * rows carry the filename — so a screen reader that announced it would read the label twice.
 */
export function icon(name: IconName, size = 14): string {
  return (
    `<svg class="ic ic-${ICON_ROLE[name]}" viewBox="${ICON_VIEWBOX}" width="${size}" height="${size}" ` +
    `fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" ` +
    `stroke-linejoin="round" aria-hidden="true">${BODY[name]}</svg>`
  );
}

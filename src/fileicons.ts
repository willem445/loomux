// Pure filename → icon mapping for the file tree (issue #174). Two halves, both
// DOM-free and node:test-covered: `iconCategory(filename)` classifies a name
// into one of a dozen buckets, and `iconSvg(category)` returns the inline SVG
// string for that bucket. Classification never throws: an unknown name always
// resolves to the generic "file" bucket.
//
// The ARTWORK half moved to src/icons.ts in #879 slice K — this module kept the
// classification, which is the part with the decisions in it, and now names a
// vendored glyph per category instead of hand-drawing one. That is also where a
// tree row gets its colour: the registry dyes each glyph by its role, so a
// listing separates code from data from folders at a glance rather than being
// fifteen shapes in one grey.

import { icon, type IconName } from "./icons.ts";

/** The box every tree row's glyph renders in — unchanged from the hand-drawn set,
 *  so the migration moves no row by a pixel. */
const TREE_ICON_PX = 14;

export type IconCategory =
  | "folder"
  | "folder-open"
  | "code"
  | "rust"
  | "python"
  | "json"
  | "markdown"
  | "web"
  | "style"
  | "shell"
  | "image"
  | "config"
  | "lock"
  | "text"
  | "file";

/** Extension (lower-cased, no dot) → category. */
const EXT_CATEGORY: Record<string, IconCategory> = {
  js: "code", mjs: "code", cjs: "code", jsx: "code",
  ts: "code", mts: "code", cts: "code", tsx: "code",
  rs: "rust",
  py: "python", pyi: "python",
  json: "json", jsonc: "json",
  md: "markdown", markdown: "markdown", mdx: "markdown",
  html: "web", htm: "web", xml: "web", svg: "web", vue: "web",
  css: "style", scss: "style", sass: "style", less: "style",
  sh: "shell", bash: "shell", zsh: "shell", fish: "shell",
  ps1: "shell", psm1: "shell", bat: "shell", cmd: "shell",
  png: "image", jpg: "image", jpeg: "image", gif: "image",
  webp: "image", bmp: "image", ico: "image", avif: "image",
  toml: "config", yaml: "config", yml: "config", ini: "config",
  cfg: "config", conf: "config", env: "config",
  lock: "lock",
  txt: "text", log: "text", csv: "text", rst: "text",
};

/** Whole-filename (lower-cased) → category, for extensionless or special files
 *  where the base name carries the meaning. */
const NAME_CATEGORY: Record<string, IconCategory> = {
  dockerfile: "config",
  makefile: "config",
  ".gitignore": "config",
  ".gitattributes": "config",
  ".editorconfig": "config",
  ".npmrc": "config",
  ".env": "config",
  "cargo.lock": "lock",
  "package-lock.json": "lock",
  "yarn.lock": "lock",
  "pnpm-lock.yaml": "lock",
  "license": "text",
  "readme": "markdown",
};

/** Classify a filename. Directories are handled by the caller (pass the dir's
 *  open/closed state to `iconSvg` directly); this is for files. Robust to
 *  uppercase, multi-dot (`a.test.ts` → its final `ts`), dotfiles (`.gitignore`),
 *  and no extension — always returns a category, never throws. */
export function iconCategory(filename: string): IconCategory {
  const lower = filename.toLowerCase();
  if (NAME_CATEGORY[lower]) return NAME_CATEGORY[lower];
  // Strip a trailing "readme"/"license" with any extension (README.md handled
  // by ext; README with none handled above; README.txt → markdown-ish is fine
  // as text via ext). Fall through to extension logic.
  const dot = lower.lastIndexOf(".");
  // No dot, or a leading-dot dotfile with no further extension (".gitignore"
  // was caught above; an unknown dotfile like ".foorc" has dot at 0) → treat the
  // segment after the dot as the ext.
  if (dot <= 0) {
    // ".foorc" → ext "foorc" (unknown → file); "Makefile" (dot < 0) → file.
    if (dot === 0) {
      const ext = lower.slice(1);
      return EXT_CATEGORY[ext] ?? "file";
    }
    return "file";
  }
  const ext = lower.slice(dot + 1);
  return EXT_CATEGORY[ext] ?? "file";
}

// ---------- category → glyph ----------
//
// Which vendored icon each bucket wears. The registry decides the COLOUR from
// the glyph's role, so this table is also the tree's legend: folders read cyan
// (workspace), authored code reads amber (source), and everything you read
// rather than run reads jade (content). Three hues in a dense listing, not
// fifteen — hue groups the kinds, shape distinguishes the members, which is the
// only way a tree of two hundred rows stays scannable.
//
// Lucide has no per-language marks, and inventing one per language is how an
// icon set ends up carrying somebody else's brand: `rust` takes the gear (its
// toolchain is the thing you actually interact with, and the glyph it replaces
// was already a gear) and `python` takes the run glyph, which is what a script
// is for. If that ever reads wrong, change the NAME here — never the artwork.
export const CATEGORY_ICON: Record<IconCategory, IconName> = {
  folder: "folder",
  "folder-open": "folder-open",
  code: "file-code",
  rust: "file-cog",
  python: "file-play",
  json: "file-braces",
  markdown: "file-type",
  web: "globe",
  style: "palette",
  shell: "file-terminal",
  image: "file-image",
  config: "file-sliders",
  lock: "file-lock",
  text: "file-text",
  file: "file",
};

/** Inline SVG string for a category, dyed by the registry's role table. */
export function iconSvg(category: IconCategory): string {
  return icon(CATEGORY_ICON[category], TREE_ICON_PX);
}

/** Convenience: the SVG for a filename in one call. */
export function fileIconSvg(filename: string): string {
  return iconSvg(iconCategory(filename));
}

/** SVG for a directory row, picking the open or closed folder glyph. */
export function folderIconSvg(open: boolean): string {
  return iconSvg(open ? "folder-open" : "folder");
}
